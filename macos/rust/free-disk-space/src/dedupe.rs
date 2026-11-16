// Replace byte-identical same-named files with APFS copy-on-write clones of one kept copy.
use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::clean::Snapshot;
use crate::meta;
use crate::scan::{finished, open_db, under, SKIP_PREFIXES};
use crate::util::{self, c, die, hms, human_i, now, warn};

// Number of duplicates a dry run lists before it only counts.
const DRY_RUN_LIST: u64 = 50;

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
    skipped: u64,
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

#[cfg(test)]
thread_local! {
    // Per-test switch that makes keep_metadata fail, standing in for an attribute macOS refuses to set.
    static FAIL_KEEP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// Give the clone everything of the target's that is not its data: mode, owner, ACL, BSD flags and every extended
// attribute value (through Apple's tool where macOS refuses, as for provenance), and none the target lacks. The
// clone keeps its own compressed state, since that describes the shared data, not the file.
fn keep_metadata(target: &Path, tmp: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_KEEP.with(std::cell::Cell::get) {
        return Err(std::io::Error::other("cannot keep extended attributes: user.tag"));
    }
    apfs::copy_metadata(target, tmp)?;
    let file = fs::File::open(tmp)?;
    // The clone's ACL does not always read back the same as the original's, so set it explicitly.
    meta::copy_acl(target, &file)?;
    meta::copy_xattrs(target, tmp)?;
    let keep = meta::xattrs(target)?;
    for name in meta::xattrs(tmp)?.into_keys().filter(|n| !keep.contains_key(n)) {
        meta::remove_xattr(tmp, &name)
            .map_err(|e| std::io::Error::other(format!("cannot remove extended attribute {name}: {e}")))?;
    }
    let have = util::flags(&file.metadata()?);
    let want =
        util::flags(&fs::symlink_metadata(target)?) & !util::UF_COMPRESSED | have & util::UF_COMPRESSED;
    if have != want {
        meta::set_flags(&file, want)?;
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

// Clone a file into the temp directory and delete the clone, to learn in advance whether macOS lets this process
// clone it (privacy protection can refuse a clone of a file it lets the process read). A file on another volume
// than the temp directory is not probed.
fn clone_probe(src: &Path, dev: u64) -> std::io::Result<()> {
    let dir = std::env::temp_dir();
    if fs::metadata(&dir)?.dev() != dev {
        return Ok(());
    }
    let tmp = dir.join(format!(".fds-clone-probe-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = apfs::clone_file(src, &tmp);
    let _ = fs::remove_file(&tmp);
    result
}

// Identical files, the copy to clone from and the copies to replace.
type Plan = (String, String, Vec<Member>);

// Hash a name group and return a plan per set of identical files on one volume, plus the files that cannot be
// cloned (path, error), which are neither cloned from nor replaced.
fn plan(
    members: Vec<Member>,
    probe: &dyn Fn(&Member) -> std::io::Result<()>,
) -> (Vec<Plan>, Vec<(String, String)>) {
    if members.len() < 2 || !members.iter().any(|m| m.replaceable) {
        return (Vec::new(), Vec::new());
    }
    let mut by_content: HashMap<(u64, String), Vec<Member>> = HashMap::new();
    for m in members {
        if let Ok(digest) = util::sha256(Path::new(&m.path)) {
            by_content.entry((m.md.dev(), digest)).or_default().push(m);
        }
    }
    let (mut plans, mut blocked) = (Vec::new(), Vec::new());
    for ((_dev, digest), same) in by_content {
        if same.len() < 2 {
            continue;
        }
        let mut same: Vec<Member> = same
            .into_iter()
            .filter(|m| match probe(m) {
                Ok(()) => true,
                Err(e) => {
                    blocked.push((m.path.clone(), e.to_string()));
                    false
                }
            })
            .collect();
        if same.len() < 2 {
            continue;
        }
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
    (plans, blocked)
}

// Clone the keeper to the temp path, give it the target's metadata with `keep`, verify both and swap it over the
// target.
fn clone_verified(
    keeper: &Path,
    target: &Path,
    tmp: &Path,
    st: &Metadata,
    digest: &str,
    keep: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<Option<String>> {
    apfs::clone_file(keeper, tmp)
        .map_err(|e| std::io::Error::new(e.kind(), format!("cloning from {}: {e}", keeper.display())))?;
    keep(target, tmp)?;
    let t = fs::symlink_metadata(tmp)?;
    let owner = |m: &Metadata| (util::perm_bits(m), m.uid(), m.gid());
    if owner(&t) != owner(st) {
        return Ok(Some("permissions/ownership differ".into()));
    }
    if !meta::same_flags(util::flags(st), util::flags(&t)) {
        return Ok(Some("file flags differ".into()));
    }
    let mut differ = meta::differing_xattrs(target, tmp)?;
    for name in meta::differing_xattrs(tmp, target)? {
        if !differ.contains(&name) {
            differ.push(name);
        }
    }
    if !differ.is_empty() {
        return Ok(Some(format!("extended attributes differ: {}", differ.join(", "))));
    }
    if meta::acl_text(target)? != meta::acl_text(tmp)? {
        return Ok(Some("ACL differs".into()));
    }
    if util::sha256(tmp)? != digest {
        return Ok(Some("content hash mismatch".into()));
    }
    util::set_times(tmp, st.atime() * 1_000_000_000 + st.atime_nsec(), util::mtime_ns(st))?;
    // Set after the other times: macOS moves a birth time later than the modification time back to it.
    if let Some(born) = meta::birth_ns(st) {
        meta::set_birth(tmp, born)?;
    }
    if !meta::same_times(st, &fs::symlink_metadata(tmp)?) {
        return Ok(Some("times differ".into()));
    }
    let st2 = fs::symlink_metadata(target)?;
    let ident = |m: &Metadata| (m.len(), util::mtime_ns(m), m.ino(), m.nlink());
    if ident(&st2) != (st.len(), util::mtime_ns(st), st.ino(), 1) {
        return Ok(Some("changed during dedupe".into()));
    }
    fs::rename(tmp, target)?;
    Ok(None)
}

// Swap one duplicate for a verified clone of the keeper and return an error note on failure.
fn clone_over(keeper: &str, target: &Member, digest: &str) -> Result<(), String> {
    let path = Path::new(&target.path);
    let tmp = util::temp_beside(path);
    let result = clone_verified(Path::new(keeper), path, &tmp, &target.md, digest, &keep_metadata);
    util::remove_temp(&tmp);
    match result {
        Ok(None) => Ok(()),
        Ok(Some(note)) => Err(note),
        Err(e) => Err(e.to_string()),
    }
}

// Clone or preview the duplicates of one name group, updating the tally, and return false to stop.
fn dedupe_group(
    db: &Connection,
    group: Vec<Member>,
    o: &DedupeOpts,
    tally: &mut Tally,
    tk: &mut util::Ticker,
) -> bool {
    let (plans, blocked) = plan(group, &|m| clone_probe(Path::new(&m.path), m.md.dev()));
    for (path, error) in blocked {
        tally.skipped += 1;
        tk.clear();
        println!("{} {path}: macOS does not let this process clone it ({error})", c("33", "SKIPPED"));
    }
    for (digest, keeper, targets) in plans {
        for target in targets {
            if util::stopped() || o.sample.is_some_and(|n| tally.files >= n) {
                return false;
            }
            let result = if o.apply { clone_over(&keeper, &target, &digest) } else { Ok(()) };
            if let Err(note) = result {
                tally.failed += 1;
                tally.streak += 1;
                tk.clear();
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
            if o.apply || tally.files <= DRY_RUN_LIST {
                tk.clear();
                let verb = if o.apply { "cloned" } else { "~ would clone" };
                println!("{verb} {:>10}  {} <- {keeper}", human_i(size), target.path);
                if !o.apply && tally.files == DRY_RUN_LIST {
                    println!("... listed the first {DRY_RUN_LIST}; still hashing, totals at the end");
                }
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
        if !dedupe_group(&db, members(rows, *size, o, &snap), o, &mut tally, &mut tk) {
            break;
        }
    }
    tk.clear();
    let free1 = util::report_free("Final  ");
    let verb = if o.apply { "Cloned" } else { "Would clone" };
    println!(
        "{verb} {} duplicate files, skipped {} that cannot be cloned, failed {} in {}",
        tally.files,
        tally.skipped,
        tally.failed,
        hms(now() - t0)
    );
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
    use crate::testkit::{age_tree, chmod, is_root, set_xattr, Sandbox};
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

    // Turn keep_metadata's failure on or off for this test thread.
    #[cfg(not(target_os = "macos"))]
    fn fail_keep(on: bool) {
        FAIL_KEEP.with(|f| f.set(on));
    }

    // Name a user attribute this platform lets a test set: macOS takes any name, Linux needs the user. prefix.
    fn tag(name: &str) -> String {
        if cfg!(target_os = "macos") {
            format!("com.example.{name}")
        } else {
            format!("user.{name}")
        }
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

    // Check the clone gets the target's mode and attribute values, loses attributes only the keeper had, and a
    // missing target is an error.
    #[test]
    fn test_keep_metadata_cases() {
        let sb = Sandbox::new();
        let (target, tmp) = (sb.write("target", b"x"), sb.write("dst.tmp", b"x"));
        chmod(&target, 0o640);
        keep_metadata(&target, &tmp).unwrap();
        assert_eq!(fs::metadata(&tmp).unwrap().mode() & 0o777, 0o640);
        assert!(keep_metadata(&sb.path("none"), &tmp).is_err());
        if !set_xattr(&target, &tag("keep"), b"target") {
            return;
        }
        assert!(set_xattr(&tmp, &tag("keep"), b"keeper") && set_xattr(&tmp, &tag("extra"), b"keeper only"));
        keep_metadata(&target, &tmp).unwrap();
        assert_eq!(meta::xattrs(&tmp).unwrap(), meta::xattrs(&target).unwrap());
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

    // Plan a group with every file cloneable, returning only the plans.
    fn plan_ok(group: Vec<Member>) -> Vec<Plan> {
        plan(group, &|_| Ok(())).0
    }

    // Check a file that cannot be cloned is never the source or a target, and the next file is used instead.
    #[test]
    fn test_plan_skips_files_that_cannot_be_cloned() {
        let sb = Sandbox::new();
        let files = dups(&sb, 3);
        fake_procs(&sb, &[]);
        let snap = Snapshot::capture();
        let rows: Vec<_> = files.iter().map(|p| (p.to_string_lossy().into_owned(), false)).collect();
        let first = files[0].to_string_lossy().into_owned();
        let refuse_first = |m: &Member| {
            if m.path == first {
                Err(std::io::Error::from_raw_os_error(libc::EPERM))
            } else {
                Ok(())
            }
        };
        let (plans, blocked) = plan(members(rows.clone(), 6000, &opts(&sb, true), &snap), &refuse_first);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].0, first);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].1, files[1].to_string_lossy());
        assert_eq!(plans[0].2.len(), 1);
        assert!(plans[0].2.iter().all(|m| m.path != first));
        let refuse_all = |_: &Member| Err(std::io::Error::from_raw_os_error(libc::EPERM));
        let (plans, blocked) = plan(members(rows, 6000, &opts(&sb, true), &snap), &refuse_all);
        assert!(plans.is_empty());
        assert_eq!(blocked.len(), 3);
        let md = fs::symlink_metadata(&files[0]).unwrap();
        clone_probe(&files[0], md.dev()).unwrap();
        assert!(clone_probe(&sb.path("missing"), md.dev()).is_err());
        clone_probe(&sb.path("missing"), md.dev() + 1).unwrap();
        let st = fs::symlink_metadata(&files[1]).unwrap();
        let tmp = util::temp_beside(&files[1]);
        let err = clone_verified(&sb.path("missing"), &files[1], &tmp, &st, "x", &keep_metadata).unwrap_err();
        assert!(err.to_string().starts_with("cloning from "), "{err}");
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
        let plans = plan_ok(build(&[&files[0], &files[1], &files[2]], false));
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].1, files[0].to_string_lossy());
        assert_eq!(plans[0].2.len(), 2);
        assert!(plan_ok(build(&[&files[0]], false)).is_empty());
        assert!(plan_ok(build(&[&files[0], &files[1]], true)).is_empty());
        let mut mixed = build(&[&files[0], &files[1]], false);
        mixed[0].replaceable = false;
        let plans = plan_ok(mixed);
        assert_eq!((plans[0].1.as_str(), plans[0].2.len()), (files[0].to_str().unwrap(), 1));
        let odd = sb.write("data/odd/same.txt", &[b'z'; 6000]);
        let mut group = build(&[&files[0], &files[1], &odd], false);
        group[0].replaceable = false;
        group[1].replaceable = false;
        let plans = plan_ok(group);
        assert!(plans.is_empty());
        let mut gone = build(&[&files[0], &files[1]], false);
        fs::remove_file(&files[0]).unwrap();
        gone.truncate(2);
        assert!(plan_ok(gone).is_empty());
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
        let go = |st: &Metadata, digest: &str, keep: &dyn Fn(&Path, &Path) -> std::io::Result<()>| {
            let r = clone_verified(keeper, target, &tmp, st, digest, keep);
            util::remove_temp(&tmp);
            r
        };
        let oth = fs::symlink_metadata(&other).unwrap();
        assert_eq!(
            go(&oth, &digest, &keep_metadata).unwrap().as_deref(),
            Some("permissions/ownership differ")
        );
        assert_eq!(go(&st(), "wrong", &keep_metadata).unwrap().as_deref(), Some("content hash mismatch"));
        let stale = st();
        util::set_times(target, 1_000_000_000_000_000_000, 1_500_000_000_000_000_000).unwrap();
        assert_eq!(go(&stale, &digest, &keep_metadata).unwrap().as_deref(), Some("changed during dedupe"));
        let unset = |_: &Path, d: &Path| util::set_times(d, 0, 0);
        assert_eq!(go(&st(), &digest, &unset).unwrap(), None, "times are set from the target after keep");
        if set_xattr(target, &tag("keep"), b"target") {
            let retagged = |t: &Path, d: &Path| {
                keep_metadata(t, d)?;
                assert!(set_xattr(d, &tag("keep"), b"keeper"));
                Ok(())
            };
            let note = format!("extended attributes differ: {}", tag("keep"));
            assert_eq!(go(&st(), &digest, &retagged).unwrap(), Some(note));
            let extra = |t: &Path, d: &Path| {
                keep_metadata(t, d)?;
                assert!(set_xattr(d, &tag("extra"), b"keeper only"));
                Ok(())
            };
            let note = format!("extended attributes differ: {}", tag("extra"));
            assert_eq!(go(&st(), &digest, &extra).unwrap(), Some(note));
        }
        assert_eq!(go(&st(), &digest, &keep_metadata).unwrap(), None);
        assert_eq!(fs::read(target).unwrap(), fs::read(keeper).unwrap());
        assert!(!tmp.exists());
        assert!(go(&st(), &digest, &keep_metadata).is_ok());
        fail_keep(true);
        assert!(go(&st(), &digest, &keep_metadata).is_err());
        fail_keep(false);
        assert!(clone_verified(&sb.path("none"), target, &tmp, &st(), &digest, &keep_metadata).is_err());
    }

    // Check on macOS that a real clone replacing a duplicate keeps the duplicate's own attribute values, ACL, flags
    // and times rather than the kept copy's, drops attributes only the kept copy has, and that a clone losing the ACL
    // or a flag fails verification.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_dedupe_keeps_macos_metadata() {
        let sb = Sandbox::new();
        let files = dups(&sb, 2);
        let (keeper, target) = (&files[0], &files[1]);
        assert!(set_xattr(keeper, &tag("tag"), b"red") && set_xattr(keeper, &tag("keeper"), b"only here"));
        assert!(set_xattr(target, &tag("tag"), b"blue"));
        let acl = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow read")
            .arg(target)
            .status();
        assert!(acl.unwrap().success());
        let hidden = 0x8000;
        let file = fs::File::open(target).unwrap();
        meta::set_flags(&file, util::flags(&file.metadata().unwrap()) | hidden).unwrap();
        let born = 1_000_000_000_000_000_000;
        meta::set_birth(target, born).unwrap();
        let before = fs::symlink_metadata(target).unwrap();
        let digest = util::sha256(keeper).unwrap();
        let tmp = util::temp_beside(target);
        let go = |keep: &dyn Fn(&Path, &Path) -> std::io::Result<()>| {
            let r = clone_verified(keeper, target, &tmp, &before, &digest, keep).unwrap();
            util::remove_temp(&tmp);
            r
        };
        let unprotected = |t: &Path, d: &Path| {
            keep_metadata(t, d)?;
            assert!(std::process::Command::new("/bin/chmod").arg("-N").arg(d).status()?.success());
            Ok(())
        };
        assert_eq!(go(&unprotected).as_deref(), Some("ACL differs"));
        let shown = |t: &Path, d: &Path| {
            keep_metadata(t, d)?;
            let f = fs::File::open(d)?;
            meta::set_flags(&f, util::flags(&f.metadata()?) & !hidden)
        };
        assert_eq!(go(&shown).as_deref(), Some("file flags differ"));
        assert_eq!(go(&keep_metadata), None);
        let md = fs::symlink_metadata(target).unwrap();
        assert_ne!(md.ino(), before.ino());
        assert_eq!(fs::read(target).unwrap(), fs::read(keeper).unwrap());
        let kept = meta::xattrs(target).unwrap();
        assert_eq!(kept.get(&tag("tag")).map(Vec::as_slice), Some(&b"blue"[..]));
        assert!(!kept.contains_key(&tag("keeper")));
        assert!(meta::acl_text(target).unwrap().unwrap().contains("allow"));
        assert_ne!(util::flags(&md) & hidden, 0);
        assert_eq!(meta::birth_ns(&md), Some(born));
        assert!(meta::same_times(&before, &md));
        assert_eq!((md.mode(), md.uid(), md.gid()), (before.mode(), before.uid(), before.gid()));
        assert_eq!(meta::xattrs(keeper).unwrap().get(&tag("tag")).map(Vec::as_slice), Some(&b"red"[..]));
    }

    // Check swapping a duplicate reports success and both failure kinds.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_clone_over_results() {
        let sb = Sandbox::new();
        let files = dups(&sb, 2);
        fake_procs(&sb, &[]);
        let snap = Snapshot::capture();
        let rows = vec![(files[1].to_string_lossy().into_owned(), false)];
        let target = members(rows, 6000, &opts(&sb, true), &snap).remove(0);
        let keeper = files[0].to_string_lossy().into_owned();
        let digest = util::sha256(&files[0]).unwrap();
        assert_eq!(clone_over(&keeper, &target, "bad"), Err("content hash mismatch".to_string()));
        assert_eq!(clone_over(&keeper, &target, &digest), Ok(()));
        fail_keep(true);
        assert!(clone_over(&keeper, &target, &digest)
            .unwrap_err()
            .contains("cannot keep extended attributes"));
        fail_keep(false);
    }

    // Check group handling in a dry run, an applied run, failures, sampling and interruption.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_dedupe_group_modes() {
        let sb = Sandbox::new();
        let files = dups(&sb, 5);
        fake_procs(&sb, &[]);
        let snap = Snapshot::capture();
        let group = |o: &DedupeOpts| {
            let rows = files.iter().map(|p| (p.to_string_lossy().into_owned(), false)).collect();
            members(rows, 6000, o, &snap)
        };
        let db = open_db(&sb.state(), false);
        let dry = opts(&sb, false);
        let mut tally = Tally::default();
        assert!(dedupe_group(&db, group(&dry), &dry, &mut tally, &mut util::Ticker::new()));
        assert_eq!((tally.files, tally.failed), (4, 0));
        let sampled = DedupeOpts { sample: Some(2), ..opts(&sb, true) };
        let mut tally = Tally::default();
        assert!(!dedupe_group(&db, group(&sampled), &sampled, &mut tally, &mut util::Ticker::new()));
        assert_eq!(tally.files, 2);
        let cloned: i64 = db.query_row("SELECT SUM(cloned) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(cloned, 2);
        fail_keep(true);
        let apply = opts(&sb, true);
        let mut tally = Tally::default();
        assert!(!dedupe_group(&db, group(&apply), &apply, &mut tally, &mut util::Ticker::new()));
        assert_eq!((tally.failed, tally.files), (3, 0));
        fail_keep(false);
        util::set_stop(true);
        assert!(!dedupe_group(&db, group(&dry), &dry, &mut Tally::default(), &mut util::Ticker::new()));
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
        assert!(dedupe_group(&db, group, &dry, &mut tally, &mut util::Ticker::new()));
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
        util::set_tty(true);
        cmd_dedupe(&opts(&sb, false));
        cmd_dedupe(&DedupeOpts { include_system: false, ..opts(&sb, false) });
        assert!(fs::metadata(&files[1]).unwrap().ino() != fs::metadata(&files[0]).unwrap().ino());
        util::set_tty(false);
        cmd_dedupe(&opts(&sb, true));
        let db = open_db(&sb.state(), false);
        let cloned: i64 = db.query_row("SELECT SUM(cloned) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(cloned, 2);
        fail_keep(true);
        cmd_dedupe(&DedupeOpts { sample: Some(1), ..opts(&sb, true) });
        fail_keep(false);
    }
}
