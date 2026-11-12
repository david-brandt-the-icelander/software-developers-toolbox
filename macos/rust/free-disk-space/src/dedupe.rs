// Replace byte-identical same-named files with APFS copy-on-write clones of one kept copy.
use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::clean::Snapshot;
use crate::scan::{finished, open_db, under, SKIP_PREFIXES};
use crate::util::{self, c, die, hms, human_i, now, warn};

const SYSTEM_PREFIXES: &[&str] = &["/Library", "/Applications", "/private", "/etc", "/var", "/tmp"];

// Options for the dedupe command.
pub struct DedupeOpts {
    pub state: String,
    pub apply: bool,
    pub include_system: bool,
    pub min_size: String,
    pub sample: Option<u64>,
}

// One file of a name group that still matches the scan.
struct Member {
    path: String,
    md: Metadata,
    replaceable: bool,
}

// Running totals across groups.
#[derive(Default)]
struct Tally {
    files: u64,
    bytes: i64,
    failed: u64,
    streak: u64,
}

// Tell whether a file is not one of the invoking user's own files.
fn is_system_file(path: &str, md: &Metadata) -> bool {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    md.uid() == 0 || md.uid() != uid || SKIP_PREFIXES.iter().chain(SYSTEM_PREFIXES).any(|p| under(path, p))
}

#[cfg(target_os = "macos")]
mod apfs {
    // Raw bindings to the macOS clone and metadata-copy calls.
    use std::path::Path;

    use crate::util::cstring;

    const CLONE_NOFOLLOW: u32 = 0x1;
    // ACL | stat | xattrs, without following symlinks.
    const COPYFILE_METADATA_NOFOLLOW: u32 = 0x7 | 0xC0000;

    extern "C" {
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32) -> libc::c_int;
        fn copyfile(
            from: *const libc::c_char,
            to: *const libc::c_char,
            state: *mut libc::c_void,
            flags: u32,
        ) -> libc::c_int;
    }

    // Clone a file copy-on-write, failing instead of falling back to a full copy.
    pub fn clone_file(src: &Path, dst: &Path) -> std::io::Result<()> {
        let (s, d) = (cstring(src)?, cstring(dst)?);
        // SAFETY: both arguments are valid NUL-terminated paths.
        if unsafe { clonefile(s.as_ptr(), d.as_ptr(), CLONE_NOFOLLOW) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    // Make a file's ACL, mode, owner, flags, times and xattrs match another file's.
    pub fn copy_metadata(src: &Path, dst: &Path) -> std::io::Result<()> {
        let (s, d) = (cstring(src)?, cstring(dst)?);
        // SAFETY: valid paths and a null state pointer, which copyfile allows.
        let rc =
            unsafe { copyfile(s.as_ptr(), d.as_ptr(), std::ptr::null_mut(), COPYFILE_METADATA_NOFOLLOW) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod apfs {
    // Plain-copy stand-ins so the dedupe flow can run under FDS_FORCE on systems without APFS clones.
    use std::path::Path;

    // Copy a file's bytes, since there is no copy-on-write clone here.
    pub fn clone_file(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::copy(src, dst).map(|_| ())
    }

    // Copy a file's permission bits.
    pub fn copy_metadata(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::set_permissions(dst, std::fs::metadata(src)?.permissions())
    }
}

// Copy metadata and drop xattrs the source lacks.
fn sync_metadata(src: &Path, dst: &Path) -> std::io::Result<()> {
    apfs::copy_metadata(src, dst)?;
    let keep = util::xattr_names(src)?;
    for name in util::xattr_names(dst)?.difference(&keep) {
        let d = dst.to_string_lossy();
        let ok = util::run(&["/usr/bin/xattr", "-d", name, &d], 60.0, &[]).is_some_and(|o| o.code == 0);
        if !ok {
            return Err(std::io::Error::other(format!("cannot remove xattr {name}")));
        }
    }
    Ok(())
}

// Return the files of one name group that still match the scan.
fn members(rows: Vec<(String, bool)>, size: i64, o: &DedupeOpts, snap: &Snapshot) -> Vec<Member> {
    let mut out = Vec::new();
    for (path, cloned) in rows {
        let Ok(md) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !md.is_file() || md.len() as i64 != size || util::flags(&md) & util::NO_TOUCH_FLAGS != 0 {
            continue;
        }
        if !o.include_system && is_system_file(&path, &md) {
            continue;
        }
        let p = Path::new(&path);
        let dir = p.parent().unwrap_or(Path::new("/"));
        let replaceable = !cloned
            && md.nlink() == 1
            && util::access(p, libc::W_OK)
            && util::access(dir, libc::W_OK | libc::X_OK)
            && !snap.has_open(&path);
        out.push(Member { path, md, replaceable });
    }
    out
}

// Hash a name group and return (digest, keeper path, targets) per set of identical files on one volume.
fn plan(members: Vec<Member>) -> Vec<(String, String, Vec<Member>)> {
    if members.len() < 2 || !members.iter().any(|m| m.replaceable) {
        return Vec::new();
    }
    let mut by_content: HashMap<(u64, String), Vec<Member>> = HashMap::new();
    for m in members {
        if let Ok(digest) = util::sha256(Path::new(&m.path)) {
            by_content.entry((m.md.dev(), digest)).or_default().push(m);
        }
    }
    let mut plans = Vec::new();
    for ((_dev, digest), mut same) in by_content {
        // Keep a file that cannot be replaced if there is one, else the oldest.
        same.sort_by(|a, b| {
            (a.replaceable, util::mtime_ns(&a.md), &a.path).cmp(&(
                b.replaceable,
                util::mtime_ns(&b.md),
                &b.path,
            ))
        });
        let keeper = same.remove(0).path;
        let targets: Vec<Member> = same.into_iter().filter(|m| m.replaceable).collect();
        if !targets.is_empty() {
            plans.push((digest, keeper, targets));
        }
    }
    plans
}

// Clone the keeper to the temp path, verify it and swap it over the target.
fn clone_verified(
    keeper: &Path,
    target: &Path,
    tmp: &Path,
    st: &Metadata,
    digest: &str,
) -> std::io::Result<Option<&'static str>> {
    apfs::clone_file(keeper, tmp)?;
    sync_metadata(target, tmp)?;
    util::set_times(tmp, st.atime() * 1_000_000_000 + st.atime_nsec(), util::mtime_ns(st))?;
    let t = fs::symlink_metadata(tmp)?;
    let owner = |m: &Metadata| (util::perm_bits(m), m.uid(), m.gid());
    if owner(&t) != owner(st) {
        return Ok(Some("permissions/ownership differ"));
    }
    if util::xattr_names(target)? != util::xattr_names(tmp)? {
        return Ok(Some("extended attributes differ"));
    }
    if util::sha256(tmp)? != digest {
        return Ok(Some("content hash mismatch"));
    }
    let st2 = fs::symlink_metadata(target)?;
    let ident = |m: &Metadata| (m.len(), util::mtime_ns(m), m.ino(), m.nlink());
    if ident(&st2) != (st.len(), util::mtime_ns(st), st.ino(), 1) {
        return Ok(Some("changed during dedupe"));
    }
    fs::rename(tmp, target)?;
    Ok(None)
}

// Swap one duplicate for a verified clone of the keeper and return an error note on failure.
fn clone_over(keeper: &str, target: &Member, digest: &str) -> Result<(), String> {
    let path = Path::new(&target.path);
    let tmp = util::temp_beside(path);
    let result = clone_verified(Path::new(keeper), path, &tmp, &target.md, digest);
    util::remove_temp(&tmp);
    match result {
        Ok(None) => Ok(()),
        Ok(Some(note)) => Err(note.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

// Clone or preview the duplicates of one name group, updating the tally, and return false to stop.
fn dedupe_group(db: &Connection, group: Vec<Member>, o: &DedupeOpts, tally: &mut Tally) -> bool {
    for (digest, keeper, targets) in plan(group) {
        for target in targets {
            if util::stopped() || o.sample.is_some_and(|n| tally.files >= n) {
                return false;
            }
            let result = if o.apply { clone_over(&keeper, &target, &digest) } else { Ok(()) };
            if let Err(note) = result {
                tally.failed += 1;
                tally.streak += 1;
                println!("{} {}: {note}", c("31", "FAILED"), target.path);
                if tally.streak >= 3 {
                    println!("{}", c("31", "3 consecutive failures - aborting for safety."));
                    return false;
                }
                continue;
            }
            if o.apply {
                let _ = db.execute("UPDATE files SET cloned=1 WHERE path=?", [&target.path]);
            }
            let size = util::alloc(&target.md);
            tally.streak = 0;
            tally.files += 1;
            tally.bytes += size;
            if o.apply || tally.files <= 50 {
                let verb = if o.apply { "cloned" } else { "~ would clone" };
                println!("{verb} {:>10}  {} <- {keeper}", human_i(size), target.path);
            }
        }
    }
    true
}

// Return name groups of same-named, same-sized files, largest first.
fn groups(db: &Connection, min_size: i64) -> Vec<(String, i64)> {
    let sql = "SELECT name, size FROM files WHERE size >= ? GROUP BY name, size HAVING COUNT(*) > 1 \
               ORDER BY size DESC";
    let Ok(mut stmt) = db.prepare(sql) else {
        return Vec::new();
    };
    stmt.query_map([min_size.max(1)], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

// Return the recorded paths and clone flags of one name group.
fn group_rows(db: &Connection, name: &str, size: i64) -> Vec<(String, bool)> {
    let Ok(mut stmt) = db.prepare("SELECT path, cloned FROM files WHERE name=? AND size=?") else {
        return Vec::new();
    };
    stmt.query_map(params![name, size], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

// Replace byte-identical same-named files with copy-on-write clones of one kept copy.
pub fn cmd_dedupe(o: &DedupeOpts) {
    if o.apply && !cfg!(target_os = "macos") && std::env::var_os("FDS_FORCE").is_none() {
        die("dedupe --apply requires macOS (APFS clonefile)");
    }
    let db = open_db(&o.state, false);
    let has_files: bool =
        db.query_row("SELECT EXISTS(SELECT 1 FROM files)", [], |r| r.get(0)).unwrap_or(false);
    if !has_files {
        die("no file names recorded in this scan state; run `new` to rescan");
    }
    if !finished(&db) {
        warn("scan is not complete; deduplicating from the file names found so far");
    }
    let groups = groups(&db, util::parse_size(&o.min_size).unwrap_or_else(|e| die(&e)));
    let mode = if o.apply { " - APPLYING" } else { " - DRY RUN (add --apply to clone)" };
    println!("{}{mode}", c("1", "free-disk-space dedupe"));
    let scope = if o.include_system { "" } else { " (system files excluded)" };
    println!("{} groups of same-named, same-sized files to hash{scope}", groups.len());
    util::install_sigint();
    let snap = Snapshot::capture();
    if o.apply && !snap.ok {
        die("could not snapshot open files with lsof; nothing was changed");
    }
    let free0 = util::report_free("Initial");
    let mut tk = util::Ticker::new();
    let mut tally = Tally::default();
    let t0 = now();
    for (i, (name, size)) in groups.iter().enumerate() {
        let short: String = name.chars().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
        tk.tick(
            &format!(
                "[{}/{}] {} {short} | {} duplicates, {} | {}",
                i + 1,
                groups.len(),
                human_i(*size),
                tally.files,
                human_i(tally.bytes),
                hms(now() - t0)
            ),
            false,
        );
        let rows = group_rows(&db, name, *size);
        tk.clear();
        if !dedupe_group(&db, members(rows, *size, o, &snap), o, &mut tally) {
            break;
        }
    }
    tk.clear();
    let free1 = util::report_free("Final  ");
    let verb = if o.apply { "Cloned" } else { "Would clone" };
    println!("{verb} {} duplicate files, failed {} in {}", tally.files, tally.failed, hms(now() - t0));
    println!(
        "Allocated size of the replaced duplicates (upper bound on savings): {}",
        c("1", &human_i(tally.bytes))
    );
    if !o.apply {
        println!("Nothing was changed. Re-run with --apply (try --sample 5 first).");
        return;
    }
    println!("Overall free-space change on the volume: {}", c("1", &human_i(free1 - free0)));
    println!("(These can differ: files that already shared blocks, snapshots and other writers affect free space.)");
}

// Unit tests for clone dedupe.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{age_tree, chmod, is_root, Sandbox};
    use std::path::PathBuf;

    // Build dedupe options for the sandbox state that include temp-dir files.
    fn opts(sb: &Sandbox, apply: bool) -> DedupeOpts {
        DedupeOpts { state: sb.state(), apply, include_system: true, min_size: "1".into(), sample: None }
    }

    // Write n identical same-named files in separate directories and record them in the state file.
    fn dups(sb: &Sandbox, n: usize) -> Vec<PathBuf> {
        let db = open_db(&sb.state(), true);
        let mut out = Vec::new();
        for i in 0..n {
            let path = sb.write(&format!("data/d{i}/same.txt"), &[b'q'; 6000]);
            age_tree(&path);
            crate::util::set_times(
                &path,
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000 + i as i64 * 1_000_000_000,
            )
            .unwrap();
            chmod(&path, 0o644);
            db.execute(
                "INSERT INTO files(path,name,size) VALUES(?,?,?)",
                params![path.to_str().unwrap(), "same.txt", 6000],
            )
            .unwrap();
            out.push(path);
        }
        out
    }

    // Install fake lsof and ps tools whose output makes the snapshot usable, listing the given open paths.
    fn fake_procs(sb: &Sandbox, open: &[&Path]) {
        let lines: String = open.iter().map(|p| format!("n{}\\n", p.display())).collect();
        sb.tool("lsof", &format!("printf 'fcwd\\nn/nowhere\\nf3\\n{lines}'"));
        sb.tool("ps", "echo idle");
    }

    #[cfg(not(target_os = "macos"))]
    // Install a fake xattr that lists the given names for the source and temp paths and removes with an exit code.
    fn fake_xattr(sb: &Sandbox, source: &str, temp: &str, delete_exit: i32) {
        sb.tool(
            "xattr",
            &format!(
                "if [ \"$1\" = \"-d\" ]; then exit {delete_exit}; fi\ncase \"$1\" in *.tmp) echo '{temp}';; *) echo '{source}';; esac"
            ),
        );
    }

    // Check the system-file test by owner and by path prefix.
    #[test]
    fn test_is_system_file_rules() {
        let sb = Sandbox::new();
        let own = sb.write("mine", b"x");
        let md = fs::symlink_metadata(&own).unwrap();
        assert_eq!(is_system_file("/home/me/f", &md), is_root());
        assert!(is_system_file("/Library/x", &md));
        assert!(is_system_file("/usr/lib/x", &md));
        let etc = fs::symlink_metadata("/etc").unwrap();
        assert!(is_system_file("/home/me/f", &etc));
    }

    // Check metadata syncing removes extra attributes and reports failures.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_sync_metadata_cases() {
        let sb = Sandbox::new();
        let (src, dst) = (sb.write("src", b"x"), sb.write("dst.tmp", b"y"));
        chmod(&src, 0o640);
        fake_xattr(&sb, "a", "a", 0);
        sync_metadata(&src, &dst).unwrap();
        assert_eq!(fs::metadata(&dst).unwrap().mode() & 0o777, 0o640);
        fake_xattr(&sb, "a", "a\nextra", 0);
        sync_metadata(&src, &dst).unwrap();
        fake_xattr(&sb, "a", "a\nextra", 1);
        assert!(sync_metadata(&src, &dst).unwrap_err().to_string().contains("cannot remove xattr extra"));
        sb.tool("xattr", "exit 1");
        assert!(sync_metadata(&src, &dst).is_err());
        assert!(sync_metadata(&sb.path("none"), &dst).is_err());
    }

    // Check which recorded files are kept and which are replaceable.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_members_filters() {
        let sb = Sandbox::new();
        let files = dups(&sb, 6);
        fake_procs(&sb, &[&files[4]]);
        let snap = Snapshot::capture();
        fs::hard_link(&files[2], sb.path("data/link")).unwrap();
        fs::remove_file(&files[5]).unwrap();
        sb.dir("data/adir");
        let mut rows: Vec<(String, bool)> =
            files.iter().map(|p| (p.to_string_lossy().into_owned(), false)).collect();
        rows[1].1 = true;
        rows.push((sb.path("data/adir").to_string_lossy().into_owned(), false));
        let out = members(rows.clone(), 6000, &opts(&sb, true), &snap);
        let flags: Vec<(String, bool)> = out
            .iter()
            .map(|m| {
                (
                    Path::new(&m.path).parent().unwrap().file_name().unwrap().to_string_lossy().into_owned(),
                    m.replaceable,
                )
            })
            .collect();
        assert_eq!(
            flags,
            [
                ("d0".into(), true),
                ("d1".into(), false),
                ("d2".into(), false),
                ("d3".into(), true),
                ("d4".into(), false)
            ]
        );
        assert!(members(rows.clone(), 1, &opts(&sb, true), &snap).is_empty());
        let strict = DedupeOpts { include_system: false, ..opts(&sb, true) };
        assert!(members(rows.clone(), 6000, &strict, &snap).is_empty());
        util::set_flags(&[util::UF_IMMUTABLE]);
        assert!(members(rows, 6000, &opts(&sb, true), &snap).is_empty());
    }

    // Check unwritable files and directories are kept but not replaced.
    #[test]
    fn test_members_unwritable() {
        let sb = Sandbox::new();
        if is_root() {
            return;
        }
        let files = dups(&sb, 2);
        fake_procs(&sb, &[]);
        chmod(&files[0], 0o444);
        chmod(files[1].parent().unwrap(), 0o555);
        let rows = files.iter().map(|p| (p.to_string_lossy().into_owned(), false)).collect();
        let out = members(rows, 6000, &opts(&sb, true), &Snapshot::capture());
        assert_eq!(out.iter().map(|m| m.replaceable).collect::<Vec<_>>(), [false, false]);
        chmod(files[1].parent().unwrap(), 0o755);
    }

    // Check planning picks keepers and targets and ignores unusable groups.
    #[test]
    fn test_plan_groups() {
        let sb = Sandbox::new();
        let files = dups(&sb, 3);
        fake_procs(&sb, &[]);
        let snap = Snapshot::capture();
        let build = |paths: &[&PathBuf], cloned: bool| {
            let rows = paths.iter().map(|p| (p.to_string_lossy().into_owned(), cloned)).collect();
            members(rows, 6000, &opts(&sb, true), &snap)
        };
        let plans = plan(build(&[&files[0], &files[1], &files[2]], false));
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].1, files[0].to_string_lossy());
        assert_eq!(plans[0].2.len(), 2);
        assert!(plan(build(&[&files[0]], false)).is_empty());
        assert!(plan(build(&[&files[0], &files[1]], true)).is_empty());
        let mut mixed = build(&[&files[0], &files[1]], false);
        mixed[0].replaceable = false;
        let plans = plan(mixed);
        assert_eq!((plans[0].1.as_str(), plans[0].2.len()), (files[0].to_str().unwrap(), 1));
        let odd = sb.write("data/odd/same.txt", &[b'z'; 6000]);
        let mut group = build(&[&files[0], &files[1], &odd], false);
        group[0].replaceable = false;
        group[1].replaceable = false;
        let plans = plan(group);
        assert!(plans.is_empty());
        let mut gone = build(&[&files[0], &files[1]], false);
        fs::remove_file(&files[0]).unwrap();
        gone.truncate(2);
        assert!(plan(gone).is_empty());
    }

    // Check every outcome of the verified clone swap.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_clone_verified_outcomes() {
        let sb = Sandbox::new();
        let files = dups(&sb, 2);
        let (keeper, target) = (&files[0], &files[1]);
        let tmp = util::temp_beside(target);
        let digest = util::sha256(keeper).unwrap();
        let other = sb.write("data/other", b"o");
        chmod(&other, 0o600);
        let st = || fs::symlink_metadata(target).unwrap();
        let go = |st: &Metadata, digest: &str| {
            let r = clone_verified(keeper, target, &tmp, st, digest);
            util::remove_temp(&tmp);
            r
        };
        fake_xattr(&sb, "", "", 0);
        assert_eq!(
            go(&fs::symlink_metadata(&other).unwrap(), &digest).unwrap(),
            Some("permissions/ownership differ")
        );
        assert_eq!(go(&st(), "wrong").unwrap(), Some("content hash mismatch"));
        let stale = st();
        util::set_times(target, 1_000_000_000_000_000_000, 1_500_000_000_000_000_000).unwrap();
        assert_eq!(go(&stale, &digest).unwrap(), Some("changed during dedupe"));
        fake_xattr(&sb, "keep", "", 0);
        assert_eq!(go(&st(), &digest).unwrap(), Some("extended attributes differ"));
        fake_xattr(&sb, "", "", 0);
        assert_eq!(go(&st(), &digest).unwrap(), None);
        assert_eq!(fs::read(target).unwrap(), fs::read(keeper).unwrap());
        assert!(!tmp.exists());
        assert!(go(&st(), &digest).is_ok());
        assert!(clone_verified(&sb.path("none"), target, &tmp, &st(), &digest).is_err());
    }

    // Check swapping a duplicate reports success and both failure kinds.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_clone_over_results() {
        let sb = Sandbox::new();
        let files = dups(&sb, 2);
        fake_procs(&sb, &[]);
        fake_xattr(&sb, "", "", 0);
        let snap = Snapshot::capture();
        let rows = vec![(files[1].to_string_lossy().into_owned(), false)];
        let target = members(rows, 6000, &opts(&sb, true), &snap).remove(0);
        let keeper = files[0].to_string_lossy().into_owned();
        let digest = util::sha256(&files[0]).unwrap();
        assert_eq!(clone_over(&keeper, &target, "bad"), Err("content hash mismatch".to_string()));
        assert_eq!(clone_over(&keeper, &target, &digest), Ok(()));
        sb.tool("xattr", "exit 1");
        assert!(clone_over(&keeper, &target, &digest).unwrap_err().contains("xattr failed"));
    }

    // Check group handling in a dry run, an applied run, failures, sampling and interruption.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_dedupe_group_modes() {
        let sb = Sandbox::new();
        let files = dups(&sb, 5);
        fake_procs(&sb, &[]);
        fake_xattr(&sb, "", "", 0);
        let snap = Snapshot::capture();
        let group = |o: &DedupeOpts| {
            let rows = files.iter().map(|p| (p.to_string_lossy().into_owned(), false)).collect();
            members(rows, 6000, o, &snap)
        };
        let db = open_db(&sb.state(), false);
        let dry = opts(&sb, false);
        let mut tally = Tally::default();
        assert!(dedupe_group(&db, group(&dry), &dry, &mut tally));
        assert_eq!((tally.files, tally.failed), (4, 0));
        let sampled = DedupeOpts { sample: Some(2), ..opts(&sb, true) };
        let mut tally = Tally::default();
        assert!(!dedupe_group(&db, group(&sampled), &sampled, &mut tally));
        assert_eq!(tally.files, 2);
        let cloned: i64 = db.query_row("SELECT SUM(cloned) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(cloned, 2);
        sb.tool("xattr", "exit 1");
        let apply = opts(&sb, true);
        let mut tally = Tally::default();
        assert!(!dedupe_group(&db, group(&apply), &apply, &mut tally));
        assert_eq!((tally.failed, tally.files), (3, 0));
        util::set_stop(true);
        assert!(!dedupe_group(&db, group(&dry), &dry, &mut Tally::default()));
    }

    // Check a dry run past fifty duplicates stops printing but keeps counting.
    #[test]
    fn test_dedupe_group_many_dry_run() {
        let sb = Sandbox::new();
        let files = dups(&sb, 53);
        fake_procs(&sb, &[]);
        let snap = Snapshot::capture();
        let rows = files.iter().map(|p| (p.to_string_lossy().into_owned(), false)).collect();
        let dry = opts(&sb, false);
        let group = members(rows, 6000, &dry, &snap);
        let db = open_db(&sb.state(), false);
        let mut tally = Tally::default();
        assert!(dedupe_group(&db, group, &dry, &mut tally));
        assert_eq!(tally.files, 52);
    }

    // Check name groups and group rows from the state file, including a broken schema.
    #[test]
    fn test_groups_and_rows() {
        let sb = Sandbox::new();
        dups(&sb, 3);
        let db = open_db(&sb.state(), false);
        db.execute("INSERT INTO files(path,name,size,cloned) VALUES('/solo','solo',9,0)", []).unwrap();
        assert_eq!(groups(&db, 1), [("same.txt".to_string(), 6000)]);
        assert!(groups(&db, 7000).is_empty());
        assert_eq!(groups(&db, 0).len(), 1);
        assert_eq!(group_rows(&db, "same.txt", 6000).len(), 3);
        db.execute_batch(
            "DROP INDEX files_name; DROP TABLE files; CREATE TABLE files(path TEXT PRIMARY KEY, x INT);
             CREATE INDEX files_name ON files(path);",
        )
        .unwrap();
        assert!(groups(&db, 1).is_empty());
        assert!(group_rows(&db, "same.txt", 6000).is_empty());
    }

    // Check the dedupe command refuses bad states and options.
    #[test]
    fn test_cmd_dedupe_guards() {
        let sb = Sandbox::new();
        let fails = |o: DedupeOpts| std::panic::catch_unwind(|| cmd_dedupe(&o)).is_err();
        assert!(fails(opts(&sb, true)));
        std::env::set_var("FDS_FORCE", "1");
        assert!(fails(opts(&sb, false)));
        drop(open_db(&sb.state(), true));
        assert!(fails(opts(&sb, false)));
        dups(&sb, 2);
        assert!(fails(DedupeOpts { min_size: "big".into(), ..opts(&sb, false) }));
        fake_procs(&sb, &[]);
        sb.tool("lsof", "true");
        assert!(fails(opts(&sb, true)));
    }

    // Check the dedupe command end to end in dry-run and applied modes.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_cmd_dedupe_runs() {
        let sb = Sandbox::new();
        std::env::set_var("FDS_FORCE", "1");
        let files = dups(&sb, 3);
        fake_procs(&sb, &[]);
        fake_xattr(&sb, "", "", 0);
        util::set_tty(true);
        cmd_dedupe(&opts(&sb, false));
        cmd_dedupe(&DedupeOpts { include_system: false, ..opts(&sb, false) });
        assert!(fs::metadata(&files[1]).unwrap().ino() != fs::metadata(&files[0]).unwrap().ino());
        util::set_tty(false);
        cmd_dedupe(&opts(&sb, true));
        let db = open_db(&sb.state(), false);
        let cloned: i64 = db.query_row("SELECT SUM(cloned) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(cloned, 2);
        sb.tool("xattr", "exit 1");
        cmd_dedupe(&DedupeOpts { sample: Some(1), ..opts(&sb, true) });
    }
}
