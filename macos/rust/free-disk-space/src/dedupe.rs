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
    // Stand-ins that fail on systems without APFS clones.
    use std::path::Path;

    // Report that cloning is unsupported here.
    pub fn clone_file(_src: &Path, _dst: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "clonefile requires macOS"))
    }

    // Report that metadata copying is unsupported here.
    pub fn copy_metadata(_src: &Path, _dst: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "copyfile requires macOS"))
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
