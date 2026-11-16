// Finds files the state file or the compressed flag falsely claim are compressed, and repairs what can be repaired.
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::compress::{self, CompressOpts};
use crate::meta;
use crate::scan::open_db;
use crate::util::{self, c, hms, now};

// Options for the repair command.
pub struct RepairOpts {
    pub compress: CompressOpts,
    pub apply: bool,
}

// What is wrong with a file that claims to be compressed.
#[derive(Debug, PartialEq)]
enum Finding {
    Unflagged,
    Broken(String),
    Wasteful,
}

// Read a decmpfs header's uncompressed size, or say why the header is missing or invalid.
fn header_size(raw: Option<&[u8]>) -> Result<u64, String> {
    let raw = raw.ok_or("no com.apple.decmpfs attribute")?;
    if raw.len() < 16 || &raw[..4] != b"fpmc" {
        return Err("invalid com.apple.decmpfs header".into());
    }
    Ok(u64::from_le_bytes(raw[8..16].try_into().unwrap_or_default()))
}

// Read a whole file through the kernel, which decompresses it, and fail where the stored data cannot be decoded.
fn read_through(path: &Path) -> std::io::Result<()> {
    let mut file = fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    while file.read(&mut buf)? > 0 {}
    Ok(())
}

// Check a flagged file: its header must match its size, its contents must read back, and its compressed form must
// take fewer disk blocks than the plain file would. Data kept inside the attribute (small files) takes no block.
fn check_flagged(path: &Path, md: &fs::Metadata) -> Option<Finding> {
    let read =
        |name: &str| meta::xattr(path, name).map_err(|e| Finding::Broken(format!("reading {name}: {e}")));
    let (raw, fork) = match (read("com.apple.decmpfs"), read("com.apple.ResourceFork")) {
        (Ok(raw), Ok(fork)) => (raw, fork),
        (Err(finding), _) | (_, Err(finding)) => return Some(finding),
    };
    match header_size(raw.as_deref()) {
        Err(note) => return Some(Finding::Broken(note)),
        Ok(size) if size != md.len() => {
            return Some(Finding::Broken("header size differs from file size".into()))
        }
        Ok(_) => {}
    }
    if let Err(e) = read_through(path) {
        return Some(Finding::Broken(format!("unreadable: {e}")));
    }
    let blocks = |bytes: u64| bytes.div_ceil(BLOCK);
    let fork_len = fork.map_or(0, |f| f.len()) as u64;
    (md.len() > 0 && blocks(fork_len) >= blocks(md.len())).then_some(Finding::Wasteful)
}

// The APFS allocation block size, in which a plain file and a resource fork take space.
const BLOCK: u64 = 4096;

// Decide what, if anything, is wrong with a file, given whether the state file records it as compressed.
fn check(path: &Path, md: &fs::Metadata, recorded_done: bool) -> Option<Finding> {
    if util::flags(md) & util::UF_COMPRESSED == 0 {
        return recorded_done.then_some(Finding::Unflagged);
    }
    check_flagged(path, md)
}

// Make `tmp` a plain, uncompressed copy of `path` with its mode, group, attributes, ACL and other flags.
fn plain_copy(path: &Path, tmp: &Path) -> std::io::Result<()> {
    let st = fs::symlink_metadata(path)?;
    let mut target = fs::OpenOptions::new().read(true).write(true).create_new(true).open(tmp)?;
    std::io::copy(&mut fs::File::open(path)?, &mut target)?;
    target.set_permissions(st.permissions())?;
    std::os::unix::fs::fchown(&target, None, Some(st.gid()))?;
    meta::copy_acl(path, &target)?;
    meta::set_flags(&target, util::flags(&st) & !util::UF_COMPRESSED)?;
    meta::copy_xattrs(path, tmp)
}

// Record a file's repair result in the state file, adding it as a candidate if the scan had not.
fn record(db: &Connection, path: &str, status: &str, note: &str, saved: i64) {
    let md = fs::symlink_metadata(path).ok();
    let (size, alloc, mtime, ino) =
        md.map_or((0, 0, 0, 0), |m| (m.len() as i64, util::alloc(&m), util::mtime_ns(&m), m.ino() as i64));
    let _ = db.execute(
        "INSERT INTO cands(path,size,alloc,est_saved,mtime_ns,ino,status,note,saved) VALUES(?,?,?,0,?,?,?,?,?)
         ON CONFLICT(path) DO UPDATE SET size=excluded.size, alloc=excluded.alloc, mtime_ns=excluded.mtime_ns,
         ino=excluded.ino, status=excluded.status, note=excluded.note, saved=excluded.saved",
        params![path, size, alloc, mtime, ino, status, note, saved],
    );
}

// Load every file the scan saw or recorded as compressed, with whether it was recorded as compressed.
fn recorded_files(db: &Connection) -> Vec<(String, bool)> {
    let sql = "SELECT path, MAX(done) FROM (SELECT path, 0 AS done FROM files
               UNION ALL SELECT path, 1 FROM cands WHERE status='done') GROUP BY path ORDER BY path";
    let Ok(mut stmt) = db.prepare(sql) else { return Vec::new() };
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? == 1)));
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
}

// Act on one finding and return the tally key it counts under.
fn repair_one(db: &Connection, path: &str, finding: Finding, o: &RepairOpts) -> &'static str {
    match finding {
        Finding::Unflagged => {
            println!("recorded as compressed but is not: {path}");
            if o.apply {
                record(db, path, "pending", "", 0);
            }
            "recorded but not compressed (reset to pending)"
        }
        Finding::Broken(note) => {
            println!("{} {path}: {note}", c("31", "BROKEN"));
            if o.apply {
                record(db, path, "broken", &note, 0);
            }
            "flagged compressed, data broken (restore from a backup)"
        }
        Finding::Wasteful if !o.apply => {
            println!("flagged compressed but saves no space: {path}");
            "flagged compressed, saves no space"
        }
        Finding::Wasteful => {
            let (status, note, _) = compress::decompress_flagged(Path::new(path), &o.compress, &plain_copy);
            match status {
                "done" => {
                    // Recorded as skipped, so compress does not pick the file up again.
                    record(db, path, "skipped", "decompressed: compression saved no space", 0);
                    "flagged compressed, saved no space: decompressed"
                }
                "skipped" => {
                    record(db, path, status, &note, 0);
                    "flagged compressed, saves no space: skipped"
                }
                _ => {
                    record(db, path, status, &note, 0);
                    println!("{} {path}: {note}", c("31", "FAILED"));
                    "flagged compressed, saves no space: decompressing failed"
                }
            }
        }
    }
}

// Check every recorded file for false compression claims and, with --apply, repair them.
pub fn cmd_repair(o: &RepairOpts) {
    let forced = std::env::var_os("FDS_FORCE").is_some();
    if !cfg!(target_os = "macos") && !forced {
        util::die("repair requires macOS (APFS transparent compression)");
    }
    let db = open_db(&o.compress.state, false);
    if o.apply {
        util::install_sigint();
    }
    let files = recorded_files(&db);
    let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
    let mut tk = util::Ticker::new();
    let t0 = now();
    for (i, (path, done)) in files.iter().enumerate() {
        if util::stopped() {
            break;
        }
        tk.tick(&format!("[{}/{}] checked | {}", i + 1, files.len(), hms(now() - t0)), false);
        let Ok(md) = fs::symlink_metadata(path) else { continue };
        if !md.is_file() {
            continue;
        }
        if let Some(finding) = check(Path::new(path), &md, *done) {
            tk.clear();
            *tally.entry(repair_one(&db, path, finding, o)).or_insert(0) += 1;
        }
    }
    tk.clear();
    println!("Checked {} files in {}", files.len(), hms(now() - t0));
    if tally.is_empty() {
        println!("No false compression claims found.");
    }
    for (what, n) in &tally {
        println!("  {n:>7}  {what}");
    }
    if !o.apply && !tally.is_empty() {
        println!("Dry run - nothing changed. Run with --apply to repair (broken files are only recorded).");
    }
    if o.apply && tally.keys().any(|k| k.contains("reset to pending")) {
        println!("Run `free-disk-space compress` to compress the files reset to pending.");
    }
}

// Unit tests for finding and repairing false compression claims.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sandbox;

    // Build repair options for the sandbox state that accept any gain and file age.
    fn opts(sb: &Sandbox, apply: bool) -> RepairOpts {
        let compress = CompressOpts {
            state: sb.state(),
            sample: None,
            max_files: None,
            limit: None,
            min_age_days: 0,
            min_gain: -1_000_000,
            reserve: 0,
            dry_run: false,
            yes: true,
        };
        RepairOpts { compress, apply }
    }

    // Create a state file recording these files as scanned, each with an optional candidate status.
    fn state(sb: &Sandbox, files: &[(&Path, Option<&str>)]) -> Connection {
        let db = open_db(&sb.state(), true);
        for (path, status) in files {
            let p = path.to_string_lossy();
            db.execute("INSERT INTO files(path,name,size) VALUES(?,'x',0)", [&p]).unwrap();
            if let Some(status) = status {
                db.execute(
                    "INSERT INTO cands(path,size,est_saved,status) VALUES(?,0,0,?)",
                    params![p, status],
                )
                .unwrap();
            }
        }
        db
    }

    // Read a file's recorded status and note.
    fn status_of(db: &Connection, path: &Path) -> (String, String) {
        let sql = "SELECT status, COALESCE(note,'') FROM cands WHERE path=?";
        db.query_row(sql, [path.to_string_lossy()], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    }

    // Check a decmpfs header is accepted only with its magic and full length.
    #[test]
    fn test_header_size() {
        assert_eq!(header_size(None).unwrap_err(), "no com.apple.decmpfs attribute");
        assert!(header_size(Some(b"fpmc")).is_err());
        assert!(header_size(Some(&[0u8; 16])).is_err());
        let mut raw = b"fpmc\x0c\0\0\0".to_vec();
        raw.extend_from_slice(&1234u64.to_le_bytes());
        assert_eq!(header_size(Some(&raw)), Ok(1234));
    }

    // Check an unflagged file is wrong only when recorded as compressed, and a flagged file without a header is broken.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_check_findings() {
        let sb = Sandbox::new();
        let file = sb.write("f", b"plain");
        let md = fs::symlink_metadata(&file).unwrap();
        util::set_flags(&[0]);
        assert_eq!(check(&file, &md, true), Some(Finding::Unflagged));
        assert_eq!(check(&file, &md, false), None);
        util::set_flags(&[util::UF_COMPRESSED]);
        assert!(matches!(check(&file, &md, false), Some(Finding::Broken(_))));
        util::set_flags(&[]);
        assert!(read_through(&sb.path("missing")).is_err());
    }

    // Check the plain copy keeps the contents, mode and attribute values, and refuses flags Linux cannot set.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_plain_copy() {
        let sb = Sandbox::new();
        let file = sb.write("f", &b"rewrite me ".repeat(1000));
        crate::testkit::chmod(&file, 0o640);
        let tagged = crate::testkit::set_xattr(&file, "user.keep", b"yes");
        let tmp = sb.path("f.tmp");
        util::set_flags(&[util::UF_COMPRESSED]);
        plain_copy(&file, &tmp).unwrap();
        assert_eq!(fs::read(&tmp).unwrap(), fs::read(&file).unwrap());
        assert_eq!(util::perm_bits(&fs::symlink_metadata(&tmp).unwrap()), 0o640);
        if tagged {
            assert_eq!(meta::xattr(&tmp, "user.keep").unwrap().as_deref(), Some(&b"yes"[..]));
        }
        assert!(plain_copy(&file, &tmp).is_err());
        fs::remove_file(&tmp).unwrap();
        util::set_flags(&[util::UF_COMPRESSED | 0x8000]);
        assert!(plain_copy(&file, &tmp).is_err());
        util::set_flags(&[]);
    }

    // Check a dry run changes nothing, and --apply resets unflagged files, records broken ones and rewrites wasteful ones.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_cmd_repair_linux() {
        let sb = Sandbox::new();
        std::env::set_var("FDS_FORCE", "1");
        sb.tool("lsof", "exit 1");
        let (a, b) = (sb.write("data/a", b"aaaa"), sb.write("data/b", b"bbbb"));
        let gone = sb.path("data/gone");
        let db = state(
            &sb,
            &[(&a, Some("done")), (&b, Some("pending")), (&gone, Some("done")), (&sb.dir("d"), None)],
        );
        util::set_flags(&[0]);
        cmd_repair(&opts(&sb, false));
        assert_eq!(status_of(&db, &a).0, "done");
        cmd_repair(&opts(&sb, true));
        assert_eq!(status_of(&db, &a).0, "pending");
        assert_eq!(status_of(&db, &b).0, "pending");
        cmd_repair(&opts(&sb, true));
        util::set_flags(&[util::UF_COMPRESSED]);
        cmd_repair(&opts(&sb, false));
        assert_eq!(status_of(&db, &a).0, "pending");
        cmd_repair(&opts(&sb, true));
        assert_eq!(status_of(&db, &a).0, "broken");
        assert!(status_of(&db, &b).1.contains("com.apple.decmpfs"));
        let text = b"wasteful ".repeat(1000);
        let w = sb.write("data/w", &text);
        crate::testkit::age_tree(&w);
        let o = opts(&sb, true);
        let dry = repair_one(&db, &w.to_string_lossy(), Finding::Wasteful, &opts(&sb, false));
        assert_eq!(dry, "flagged compressed, saves no space");
        // Flags read: precheck and the copy see the original as compressed, the copy reads plain, the original again.
        util::set_flags(&[util::UF_COMPRESSED, util::UF_COMPRESSED, 0, util::UF_COMPRESSED]);
        let what = repair_one(&db, &w.to_string_lossy(), Finding::Wasteful, &o);
        assert!(what.ends_with("decompressed"), "{what}");
        assert_eq!(
            status_of(&db, &w),
            ("skipped".to_string(), "decompressed: compression saved no space".to_string())
        );
        assert_eq!(fs::read(&w).unwrap(), text);
        let missing = sb.path("data/missing").to_string_lossy().into_owned();
        assert!(repair_one(&db, &missing, Finding::Wasteful, &o).ends_with("skipped"));
        let nodir = sb.dir("data/locked");
        let locked = sb.write("data/locked/l", &text);
        crate::testkit::chmod(&nodir, 0o555);
        let what = repair_one(&db, &locked.to_string_lossy(), Finding::Wasteful, &o);
        crate::testkit::chmod(&nodir, 0o755);
        if !crate::testkit::is_root() {
            assert!(what.ends_with("skipped"));
        }
        util::set_flags(&[util::UF_COMPRESSED, 0x8000]);
        let what = repair_one(&db, &w.to_string_lossy(), Finding::Wasteful, &o);
        assert!(what.ends_with("decompressing failed"), "{what}");
        util::set_flags(&[]);
    }

    // Turn a file into a flagged compressed file with this decmpfs type and resource fork, as the library does.
    #[cfg(target_os = "macos")]
    fn flag_as_compressed(path: &Path, kind: u32, fork: Option<&[u8]>) {
        let len = fs::metadata(path).unwrap().len();
        if let Some(fork) = fork {
            meta::set_xattr(path, "com.apple.ResourceFork", fork).unwrap();
        }
        let mut header = b"fpmc".to_vec();
        header.extend_from_slice(&kind.to_le_bytes());
        header.extend_from_slice(&len.to_le_bytes());
        meta::set_xattr(path, "com.apple.decmpfs", &header).unwrap();
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_len(0).unwrap();
        let flags = util::flags(&file.metadata().unwrap());
        meta::set_flags(&file, flags | util::UF_COMPRESSED).unwrap();
    }

    // Check on macOS that repair resets a falsely recorded file, records a broken one and really compresses a flagged
    // file whose LZVN chunks are all stored raw.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_cmd_repair_macos() {
        let sb = Sandbox::new();
        sb.tool("lsof", "exit 1");
        let text = b"stored raw, so this flagged file saves nothing. ".repeat(4000);
        let raw = sb.write("data/raw", &text);
        let chunks: Vec<&[u8]> = text.chunks(65536).collect();
        let mut fork = Vec::new();
        let mut offset = 4 * (chunks.len() as u32 + 1);
        fork.extend_from_slice(&offset.to_le_bytes());
        for chunk in &chunks {
            offset += chunk.len() as u32 + 1;
            fork.extend_from_slice(&offset.to_le_bytes());
        }
        for chunk in &chunks {
            fork.push(0x06);
            fork.extend_from_slice(chunk);
        }
        flag_as_compressed(&raw, 8, Some(&fork));
        assert_eq!(fs::read(&raw).unwrap(), text, "the kernel must read the raw-chunk file");
        let md = fs::symlink_metadata(&raw).unwrap();
        assert_eq!(check(&raw, &md, false), Some(Finding::Wasteful));
        let broken = sb.write("data/broken", &text);
        flag_as_compressed(&broken, 8, None);
        let unflagged = sb.write("data/plain", &text);
        let db = state(&sb, &[(&raw, None), (&broken, None), (&unflagged, Some("done"))]);
        cmd_repair(&opts(&sb, true));
        assert_eq!(status_of(&db, &unflagged).0, "pending");
        assert_eq!(status_of(&db, &broken).0, "broken");
        let decompressed = ("skipped".to_string(), "decompressed: compression saved no space".to_string());
        assert_eq!(status_of(&db, &raw), decompressed);
        let md = fs::symlink_metadata(&raw).unwrap();
        assert_eq!(util::flags(&md) & util::UF_COMPRESSED, 0);
        assert_eq!(fs::read(&raw).unwrap(), text);
        assert_eq!(check(&raw, &md, false), None);
        // A small file compressed inside its attribute takes no disk block, so it saves space and is left alone.
        let small = sb.write("data/small.strings", b"\"Hello\" = \"Hello\";\n");
        let target = sb.write("data/small-compressed.strings", b"\"Hello\" = \"Hello\";\n");
        let file = fs::OpenOptions::new().read(true).write(true).open(&target).unwrap();
        crate::lzfse::compress_onto(&fs::File::open(&small).unwrap(), &file).unwrap();
        let md = fs::symlink_metadata(&target).unwrap();
        assert_ne!(util::flags(&md) & util::UF_COMPRESSED, 0);
        assert_eq!(check(&target, &md, false), None);
    }
}
