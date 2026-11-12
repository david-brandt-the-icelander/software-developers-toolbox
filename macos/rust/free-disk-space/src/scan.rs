// Resumable compression-candidate scan stored in SQLite, plus status and list commands.
use std::fs::{self, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::util::{self, c, die, hms, human_i, now};

// Seconds between progress commits during a scan.
const COMMIT_EVERY: f64 = 2.0;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS dirs(id INTEGER PRIMARY KEY, path TEXT UNIQUE, done INTEGER DEFAULT 0);
CREATE INDEX IF NOT EXISTS dirs_done ON dirs(done);
CREATE TABLE IF NOT EXISTS cands(
  path TEXT PRIMARY KEY, size INTEGER, alloc INTEGER, est_saved INTEGER,
  mtime_ns INTEGER, ino INTEGER, status TEXT DEFAULT 'pending', note TEXT, saved INTEGER DEFAULT 0);
CREATE INDEX IF NOT EXISTS cands_status ON cands(status, est_saved);
CREATE TABLE IF NOT EXISTS files(
  path TEXT PRIMARY KEY, name TEXT, size INTEGER, cloned INTEGER DEFAULT 0) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS files_name ON files(name, size);
";

// Extensions of already-compressed media, live databases, VM disks and credentials.
const SKIP_EXT: &[&str] = &[
    "zip",
    "gz",
    "tgz",
    "bz2",
    "xz",
    "zst",
    "7z",
    "rar",
    "lz4",
    "lzma",
    "br",
    "jar",
    "war",
    "apk",
    "ipa",
    "dmg",
    "iso",
    "jpg",
    "jpeg",
    "png",
    "gif",
    "heic",
    "heif",
    "webp",
    "avif",
    "mp3",
    "m4a",
    "aac",
    "flac",
    "ogg",
    "opus",
    "mp4",
    "m4v",
    "mov",
    "mkv",
    "avi",
    "webm",
    "wmv",
    "pack",
    "rpm",
    "deb",
    "whl",
    "woff2",
    "sqlite",
    "sqlite3",
    "db",
    "db3",
    "sqlite-wal",
    "sqlite-shm",
    "db-wal",
    "db-shm",
    "ldb",
    "realm",
    "mdb",
    "ibd",
    "raw",
    "img",
    "vmdk",
    "vdi",
    "vhd",
    "vhdx",
    "qcow2",
    "sparseimage",
    "sparsebundle",
    "lock",
    "pem",
    "key",
    "p12",
    "pfx",
    "keychain",
    "keychain-db",
    "gpg",
    "kdbx",
];
const SKIP_NAMES: &[&str] =
    &[".DS_Store", "Docker.raw", "id_rsa", "id_ed25519", "id_ecdsa", ".netrc", ".npmrc", ".pypirc"];
const SKIP_DIR_SUFFIX: &[&str] = &[
    ".app",
    ".photoslibrary",
    ".vmwarevm",
    ".pvm",
    ".utm",
    ".sparsebundle",
    ".tvlibrary",
    ".musiclibrary",
    ".imovielibrary",
    ".fcpbundle",
    ".xcarchive",
];
const SKIP_DIR_NAMES: &[&str] = &[
    ".Trash",
    ".Spotlight-V100",
    ".fseventsd",
    ".DocumentRevisions-V100",
    ".TemporaryItems",
    ".ssh",
    ".gnupg",
    ".aws",
    ".gcloud",
    "Keychains",
    "Mobile Documents",
    "CloudStorage",
    "com.docker.docker",
];
pub const SKIP_PREFIXES: &[&str] = &[
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/dev",
    "/Volumes",
    "/cores",
    "/.vol",
    "/private/var/vm",
    "/private/var/db",
    "/System/Volumes/VM",
    "/System/Volumes/Preboot",
    "/System/Volumes/Update",
    "/System/Volumes/Hardware",
    "/System/Volumes/iSCPreboot",
    "/Library/Apple",
];

// Settings a scan was started with.
pub struct Cfg {
    pub root: String,
    dev: u64,
    min_size: i64,
    min_age_days: i64,
    min_pct: i64,
    cross: bool,
    exclude: Vec<String>,
}

impl Cfg {
    // Convert the settings to JSON for the meta table.
    fn to_json(&self) -> Value {
        json!({
            "root": self.root, "dev": self.dev, "min_size": self.min_size,
            "min_age_days": self.min_age_days, "min_pct": self.min_pct, "cross": self.cross,
            "exclude": self.exclude,
        })
    }

    // Read the settings back from JSON.
    fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            root: v["root"].as_str()?.to_string(),
            dev: v["dev"].as_u64()?,
            min_size: v["min_size"].as_i64()?,
            min_age_days: v["min_age_days"].as_i64()?,
            min_pct: v["min_pct"].as_i64()?,
            cross: v["cross"].as_bool()?,
            exclude: v["exclude"].as_array()?.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        })
    }
}

// Running totals of a scan.
#[derive(Default)]
struct Stats {
    dirs: i64,
    files: i64,
    bytes: i64,
    errors: i64,
    cands: i64,
    est: i64,
}

impl Stats {
    // Convert the totals to JSON.
    fn to_json(&self) -> Value {
        json!({"dirs": self.dirs, "files": self.files, "bytes": self.bytes,
               "errors": self.errors, "cands": self.cands, "est": self.est})
    }

    // Read totals from JSON, defaulting missing fields to zero.
    fn from_json(v: &Value) -> Self {
        let get = |k: &str| v[k].as_i64().unwrap_or(0);
        Self {
            dirs: get("dirs"),
            files: get("files"),
            bytes: get("bytes"),
            errors: get("errors"),
            cands: get("cands"),
            est: get("est"),
        }
    }
}

// Open (or create) the scan state database.
pub fn open_db(path: &str, create: bool) -> Connection {
    if !create && !Path::new(path).exists() {
        die(&format!("no scan state at {path} - start one with: free-disk-space new"));
    }
    if let Some(parent) = Path::new(path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let open = || -> rusqlite::Result<Connection> {
        let db = Connection::open(path)?;
        db.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
        db.execute_batch("PRAGMA synchronous=NORMAL;")?;
        db.execute_batch(SCHEMA)?;
        Ok(db)
    };
    open().unwrap_or_else(|e| die(&format!("cannot open {path}: {e}")))
}

// Read a JSON value from the meta table.
pub fn meta_get(db: &Connection, key: &str) -> Option<Value> {
    let text: Option<String> =
        db.query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0)).optional().ok().flatten();
    text.and_then(|t| serde_json::from_str(&t).ok())
}

// Write a JSON value to the meta table.
fn meta_set(db: &Connection, key: &str, value: &Value) {
    let _ = db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?,?)", params![key, value.to_string()]);
}

// Tell whether the scan has finished.
pub fn finished(db: &Connection) -> bool {
    meta_get(db, "finished").is_some_and(|v| !v.is_null())
}

// Count directories still waiting to be scanned.
fn pending_dirs(db: &Connection) -> i64 {
    db.query_row("SELECT COUNT(*) FROM dirs WHERE done=0", [], |r| r.get(0)).unwrap_or(0)
}

// Estimate compressed/raw size from head, middle and tail samples of a file.
fn sample_ratio(path: &Path, size: u64) -> Option<f64> {
    const CHUNK: u64 = 64 * 1024;
    let mut f = fs::File::open(path).ok()?;
    let mut data = Vec::new();
    if size <= 4 * CHUNK {
        f.read_to_end(&mut data).ok()?;
    } else {
        for off in [0, size / 2 - CHUNK / 2, size - CHUNK] {
            f.seek(SeekFrom::Start(off)).ok()?;
            let mut part = Vec::new();
            (&mut f).take(CHUNK).read_to_end(&mut part).ok()?;
            data.extend_from_slice(&part);
        }
    }
    if data.is_empty() {
        return None;
    }
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(3));
    std::io::Write::write_all(&mut enc, &data).ok()?;
    Some(enc.finish().ok()?.len() as f64 / data.len() as f64)
}

// Return estimated savings if a file is a compression candidate, else None.
fn consider(path: &Path, name: &str, md: &Metadata, cfg: &Cfg, cutoff: f64) -> Option<i64> {
    let size = md.len() as i64;
    if !md.is_file() || size < cfg.min_size || md.nlink() != 1 {
        return None;
    }
    if util::flags(md) & util::NO_TOUCH_FLAGS != 0 || md.mtime() as f64 > cutoff {
        return None;
    }
    // Sparse files are already smaller on disk.
    if (util::alloc(md) as f64) < size as f64 * 0.8 {
        return None;
    }
    if SKIP_NAMES.contains(&name) || name.starts_with("id_") {
        return None;
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    if SKIP_EXT.contains(&ext.as_str()) {
        return None;
    }
    let ratio = sample_ratio(path, md.len())?;
    let est = (size as f64 * (1.0 - ratio) * 0.85) as i64;
    if (1.0 - ratio) * 100.0 < cfg.min_pct as f64 || est < 128 * 1024 {
        return None;
    }
    Some(est)
}

// Tell whether a path equals a prefix or lies beneath it.
pub fn under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

// Tell whether a directory must not be scanned.
fn dir_excluded(path: &str, name: &str, cfg: &Cfg) -> bool {
    SKIP_DIR_NAMES.contains(&name)
        || SKIP_DIR_SUFFIX.iter().any(|s| name.ends_with(s))
        || SKIP_PREFIXES.iter().any(|p| under(path, p))
        || cfg.exclude.iter().any(|p| under(path, p))
}

// Scan one directory entry, queueing subdirectories and recording files and candidates.
fn scan_entry(
    db: &Connection,
    entry: &fs::DirEntry,
    cfg: &Cfg,
    cutoff: f64,
    stats: &mut Stats,
) -> Result<(), ()> {
    let path = entry.path();
    let path_str = path.to_str().ok_or(())?;
    let name = entry.file_name().into_string().map_err(|_| ())?;
    let kind = entry.file_type().map_err(|_| ())?;
    if kind.is_symlink() {
        return Ok(());
    }
    if kind.is_dir() {
        if dir_excluded(path_str, &name, cfg) {
            return Ok(());
        }
        if !cfg.cross && entry.metadata().map_err(|_| ())?.dev() != cfg.dev {
            return Ok(());
        }
        db.execute("INSERT OR IGNORE INTO dirs(path) VALUES(?)", [path_str]).map_err(|_| ())?;
        return Ok(());
    }
    if !kind.is_file() {
        return Ok(());
    }
    let md = entry.metadata().map_err(|_| ())?;
    stats.files += 1;
    stats.bytes += md.len() as i64;
    db.execute(
        "INSERT OR IGNORE INTO files(path,name,size) VALUES(?,?,?)",
        params![path_str, name, md.len() as i64],
    )
    .map_err(|_| ())?;
    let Some(est) = consider(&path, &name, &md, cfg, cutoff) else {
        return Ok(());
    };
    let added = db
        .execute(
            "INSERT OR IGNORE INTO cands(path,size,alloc,est_saved,mtime_ns,ino) VALUES(?,?,?,?,?,?)",
            params![path_str, md.len() as i64, util::alloc(&md), est, util::mtime_ns(&md), md.ino() as i64],
        )
        .map_err(|_| ())?;
    if added > 0 {
        stats.cands += 1;
        stats.est += est;
    }
    Ok(())
}

// Process pending directories until done or interrupted and return true if finished.
fn run_scan(db: &Connection, cfg: &Cfg, commit_every: f64) -> bool {
    let mut stats = meta_get(db, "stats").map(|v| Stats::from_json(&v)).unwrap_or_default();
    let prior = meta_get(db, "elapsed").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cutoff = now() - cfg.min_age_days as f64 * 86400.0;
    let t0 = now();
    let mut tk = util::Ticker::new();
    let mut last_commit = now();
    let commit = |stats: &Stats| {
        meta_set(db, "stats", &stats.to_json());
        meta_set(db, "elapsed", &json!(prior + now() - t0));
        let _ = db.execute_batch("COMMIT; BEGIN");
    };
    let _ = db.execute_batch("BEGIN");
    while !util::stopped() {
        let row: Option<(i64, String)> = db
            .query_row("SELECT id, path FROM dirs WHERE done=0 ORDER BY id DESC LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .unwrap_or(None);
        let Some((id, dir)) = row else {
            break;
        };
        match fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries {
                    let ok = entry.map_err(|_| ()).and_then(|e| scan_entry(db, &e, cfg, cutoff, &mut stats));
                    if ok.is_err() {
                        stats.errors += 1;
                    }
                }
            }
            Err(_) => stats.errors += 1,
        }
        let _ = db.execute("UPDATE dirs SET done=1 WHERE id=?", [id]);
        stats.dirs += 1;
        if now() - last_commit > commit_every {
            commit(&stats);
            last_commit = now();
        }
        let shown = if util::is_tty() {
            dir.clone()
        } else {
            dir.chars().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect()
        };
        tk.tick(
            &format!(
                "scanned {} dirs ({} queued) | {} files, {} | {} candidates, ~{} saveable | {} | {shown}",
                stats.dirs,
                pending_dirs(db),
                stats.files,
                human_i(stats.bytes),
                stats.cands,
                human_i(stats.est),
                hms(prior + now() - t0)
            ),
            false,
        );
    }
    commit(&stats);
    let _ = db.execute_batch("COMMIT");
    tk.clear();
    if pending_dirs(db) == 0 {
        meta_set(db, "finished", &json!(now()));
        return true;
    }
    false
}

// Remove a state database and its WAL files.
fn remove_state(path: &str) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{path}{suffix}"));
    }
}

// Options for starting a new scan.
pub struct NewOpts {
    pub state: String,
    pub root: String,
    pub min_size: String,
    pub min_age_days: i64,
    pub min_savings_pct: i64,
    pub cross_devices: bool,
    pub exclude: Vec<String>,
    pub force: bool,
}

// Start a fresh scan and run it.
pub fn cmd_new(o: &NewOpts) {
    let root = util::realpath(&o.root);
    let Ok(root_md) = fs::metadata(&root) else {
        die(&format!("root {root} is not a directory"));
    };
    if !root_md.is_dir() {
        die(&format!("root {root} is not a directory"));
    }
    if Path::new(&o.state).exists() {
        let unfinished = pending_dirs(&open_db(&o.state, false));
        if unfinished > 0 && !o.force {
            die(&format!(
                "an unfinished scan exists ({unfinished} dirs pending). \
                 Use `resume`, or `new --force` to discard it."
            ));
        }
        remove_state(&o.state);
    }
    let db = open_db(&o.state, true);
    let cfg = Cfg {
        root: root.clone(),
        dev: root_md.dev(),
        min_size: util::parse_size(&o.min_size).unwrap_or_else(|e| die(&e)),
        min_age_days: o.min_age_days,
        min_pct: o.min_savings_pct,
        cross: o.cross_devices,
        exclude: o.exclude.iter().map(|x| util::realpath(x)).collect(),
    };
    meta_set(&db, "cfg", &cfg.to_json());
    meta_set(&db, "started", &json!(now()));
    let _ = db.execute("INSERT OR IGNORE INTO dirs(path) VALUES(?)", [&root]);
    println!(
        "Scanning {root}  (min size {}, untouched >= {} days, >= {}% estimated savings)",
        human_i(cfg.min_size),
        cfg.min_age_days,
        cfg.min_pct
    );
    println!("State: {}   (Ctrl-C is safe; continue with `resume`)", o.state);
    finish_scan(&db, &cfg);
}

// Run the scan loop and report whether it completed or paused.
fn finish_scan(db: &Connection, cfg: &Cfg) {
    util::install_sigint();
    let done = run_scan(db, cfg, COMMIT_EVERY);
    if !done {
        println!("{}Run `resume` to continue.", c("33", "Scan paused. "));
        return;
    }
    let s = meta_get(db, "stats").map(|v| Stats::from_json(&v)).unwrap_or_default();
    println!("{}{} candidates, ~{} estimated saveable.", c("32", "Scan complete: "), s.cands, human_i(s.est));
    println!("Next: `dedupe` to preview duplicate cloning, `list` then `compress --sample 5` to compress.");
}

// Continue an interrupted scan.
pub fn cmd_resume(state: &str) {
    let db = open_db(state, false);
    let Some(cfg) = meta_get(&db, "cfg").and_then(|v| Cfg::from_json(&v)) else {
        die("state file has no scan configuration; start with `new`");
    };
    if finished(&db) {
        println!("Scan already complete. Use `new` to rescan, or `status`/`compress`.");
        return;
    }
    println!("Resuming scan of {} ({} directories pending)", cfg.root, pending_dirs(&db));
    finish_scan(&db, &cfg);
}

// Show scan progress, candidate totals and free space.
pub fn cmd_status(state: &str) {
    let db = open_db(state, false);
    let cfg = meta_get(&db, "cfg").unwrap_or(Value::Null);
    let s = meta_get(&db, "stats").map(|v| Stats::from_json(&v)).unwrap_or_default();
    let elapsed = meta_get(&db, "elapsed").and_then(|v| v.as_f64()).unwrap_or(0.0);
    println!("{}: root {}", c("1", "Scan"), cfg["root"].as_str().unwrap_or("None"));
    let state_text = if finished(&db) {
        "complete".to_string()
    } else {
        format!("in progress ({} dirs queued)", pending_dirs(&db))
    };
    println!("  state     : {state_text}");
    println!(
        "  processed : {} dirs, {} files, {}, {} errors, {} elapsed",
        s.dirs,
        s.files,
        human_i(s.bytes),
        s.errors,
        hms(elapsed)
    );
    println!("{}", c("1", "Candidates"));
    let sql =
        "SELECT status, COUNT(*), SUM(est_saved), SUM(saved) FROM cands GROUP BY status ORDER BY status";
    if let Ok(mut stmt) = db.prepare(sql) {
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        });
        for (status, n, est, saved) in rows.into_iter().flatten().flatten() {
            println!(
                "  {status:<8} {n:>7} files   estimated {:<10} verified saved {}",
                human_i(est.unwrap_or(0)),
                human_i(saved.unwrap_or(0))
            );
        }
    }
    let (names, cloned): (i64, Option<i64>) = db
        .query_row("SELECT COUNT(*), SUM(cloned) FROM files", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((0, None));
    println!("{}: {names} recorded, {} replaced by clones", c("1", "File names"), cloned.unwrap_or(0));
    util::report_free("Current");
}

// List the best candidates by estimated savings.
pub fn cmd_list(state: &str, status: &str, top: i64) {
    let db = open_db(state, false);
    let sql = "SELECT path,size,est_saved,note FROM cands WHERE status=? ORDER BY est_saved DESC LIMIT ?";
    let mut stmt = db.prepare(sql).unwrap_or_else(|e| die(&e.to_string()));
    let rows: Vec<(String, i64, i64, Option<String>)> = stmt
        .query_map(params![status, top], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default();
    for (path, size, est, note) in &rows {
        let note = match note.as_deref() {
            Some(n) if !n.is_empty() => format!("  [{n}]"),
            _ => String::new(),
        };
        println!("{:>10}  ~{:>10}  {path}{note}", human_i(*size), human_i(*est));
    }
    if rows.is_empty() {
        println!("(none with status {status})");
    }
}

// Unit tests for scan filtering and the scan commands.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{chmod, is_root, Sandbox};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // Build scan settings rooted at a path that accept any small old file.
    fn cfg(root: &Path) -> Cfg {
        let dev = fs::metadata(root).map(|m| m.dev()).unwrap_or(0);
        Cfg {
            root: root.to_string_lossy().into_owned(),
            dev,
            min_size: 1,
            min_age_days: 0,
            min_pct: 25,
            cross: false,
            exclude: Vec::new(),
        }
    }

    // Build options for a new scan of a root with the sandbox state file.
    fn new_opts(sb: &Sandbox, root: &Path) -> NewOpts {
        NewOpts {
            state: sb.state(),
            root: root.to_string_lossy().into_owned(),
            min_size: "1".into(),
            min_age_days: 0,
            min_savings_pct: 25,
            cross_devices: false,
            exclude: Vec::new(),
            force: false,
        }
    }

    // Create a file with a non-UTF-8 name and report whether the filesystem allowed it.
    fn write_non_utf8(dir: &Path) -> bool {
        fs::write(dir.join(OsStr::from_bytes(b"bad\xff")), b"x").is_ok()
    }

    // Find one directory entry by name.
    fn entry(dir: &Path, name: &str) -> fs::DirEntry {
        fs::read_dir(dir).unwrap().flatten().find(|e| e.file_name() == name).expect("entry")
    }

    // Check prefix matching respects path boundaries.
    #[test]
    fn test_under_boundaries() {
        assert!(under("/System/x", "/System"));
        assert!(under("/usr", "/usr/"));
        assert!(!under("/usrlocal", "/usr"));
    }

    // Check that highly compressible data gets a low ratio and bad inputs give none.
    #[test]
    fn test_sample_ratio_compressible() {
        let sb = Sandbox::new();
        let small = sb.write("small", &vec![b'a'; 1 << 20]);
        assert!(sample_ratio(&small, 1 << 20).unwrap() < 0.05);
        let tiny = sb.write("tiny", b"hello hello hello hello");
        assert!(sample_ratio(&tiny, 23).unwrap() > 0.0);
        let mut noise = vec![0u8; 1 << 19];
        let mut x = 12345u32;
        for b in noise.iter_mut() {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (x >> 24) as u8;
        }
        let big = sb.write("big", &noise);
        assert!(sample_ratio(&big, noise.len() as u64).unwrap() > 0.9);
        let empty = sb.write("empty", b"");
        assert!(sample_ratio(&empty, 0).is_none());
        assert!(sample_ratio(&sb.path("missing"), 10).is_none());
        let dir = sb.dir("adir");
        assert!(sample_ratio(&dir, 10).is_none());
    }

    // Check settings and totals survive a JSON round trip and reject partial settings.
    #[test]
    fn test_cfg_stats_json() {
        let sb = Sandbox::new();
        let mut c = cfg(&sb.root);
        c.exclude = vec!["/x".into()];
        let back = Cfg::from_json(&c.to_json()).unwrap();
        assert_eq!((back.root, back.min_size, back.exclude), (c.root.clone(), 1, vec!["/x".to_string()]));
        assert!(Cfg::from_json(&json!({"root": "/"})).is_none());
        let stats = Stats { dirs: 1, files: 2, bytes: 3, errors: 4, cands: 5, est: 6 };
        let back = Stats::from_json(&stats.to_json());
        assert_eq!(
            (back.dirs, back.files, back.bytes, back.errors, back.cands, back.est),
            (1, 2, 3, 4, 5, 6)
        );
        assert_eq!(Stats::from_json(&json!({})).files, 0);
    }

    // Check opening the state database creates, reuses and rejects paths.
    #[test]
    fn test_open_db_cases() {
        let sb = Sandbox::new();
        let path = sb.path("nested/dir/state.sqlite3").to_string_lossy().into_owned();
        drop(open_db(&path, true));
        drop(open_db("", true));
        let db = open_db(&path, false);
        meta_set(&db, "k", &json!({"a": 1}));
        assert_eq!(meta_get(&db, "k"), Some(json!({"a": 1})));
        assert_eq!(meta_get(&db, "absent"), None);
        assert!(!finished(&db));
        meta_set(&db, "finished", &Value::Null);
        assert!(!finished(&db));
        meta_set(&db, "finished", &json!(5));
        assert!(finished(&db));
        db.execute("INSERT INTO meta(key,value) VALUES('bad','{')", []).unwrap();
        assert_eq!(meta_get(&db, "bad"), None);
        let missing = sb.path("missing.sqlite3").to_string_lossy().into_owned();
        assert!(std::panic::catch_unwind(|| open_db(&missing, false)).is_err());
        let as_dir = sb.dir("isdir").to_string_lossy().into_owned();
        assert!(std::panic::catch_unwind(|| open_db(&as_dir, true)).is_err());
    }

    // Check every reason a file is rejected as a candidate, and the accepted case.
    #[test]
    fn test_consider_decisions() {
        let sb = Sandbox::new();
        let c = cfg(&sb.root);
        let cutoff = util::now() + 100.0;
        let check = |name: &str, c: &Cfg, cutoff: f64| {
            let p = sb.path(name);
            consider(&p, name, &fs::symlink_metadata(&p).unwrap(), c, cutoff)
        };
        sb.write("good.txt", &vec![b'a'; 1 << 20]);
        assert!(check("good.txt", &c, cutoff).unwrap() > 128 * 1024);
        assert!(check("good.txt", &c, 1.0).is_none());
        sb.dir("adir");
        assert!(check("adir", &c, cutoff).is_none());
        let big_min = Cfg { min_size: 1 << 30, ..cfg(&sb.root) };
        assert!(check("good.txt", &big_min, cutoff).is_none());
        fs::hard_link(sb.path("good.txt"), sb.path("linked.txt")).unwrap();
        assert!(check("linked.txt", &c, cutoff).is_none());
        sb.write("pic.JPG", &vec![b'a'; 1 << 20]);
        assert!(check("pic.JPG", &c, cutoff).is_none());
        sb.write("id_rsa", &vec![b'a'; 1 << 20]);
        sb.write("id_other", &vec![b'a'; 1 << 20]);
        sb.write(".DS_Store", &vec![b'a'; 1 << 20]);
        assert!(check("id_rsa", &c, cutoff).is_none());
        assert!(check("id_other", &c, cutoff).is_none());
        assert!(check(".DS_Store", &c, cutoff).is_none());
        sb.write("noext", &vec![b'a'; 1 << 20]);
        assert!(check("noext", &c, cutoff).is_some());
        let sparse = fs::File::create(sb.path("sparse.txt")).unwrap();
        sparse.set_len(1 << 24).unwrap();
        assert!(check("sparse.txt", &c, cutoff).is_none());
        sb.write("empty.txt", b"");
        assert!(check("empty.txt", &c, cutoff).is_none());
        let picky = Cfg { min_pct: 99, ..cfg(&sb.root) };
        let mut noise = vec![0u8; 1 << 19];
        let mut x = 7u32;
        for b in noise.iter_mut() {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (x >> 24) as u8;
        }
        sb.write("noise.txt", &noise);
        assert!(check("noise.txt", &picky, cutoff).is_none());
        sb.write("small.txt", &vec![b'a'; 4000]);
        assert!(check("small.txt", &c, cutoff).is_none());
        #[cfg(not(target_os = "macos"))]
        {
            sb.write("flagged.txt", &vec![b'a'; 1 << 20]);
            assert!(check("flagged.txt", &c, cutoff).is_some());
            util::set_flags(&[util::UF_COMPRESSED]);
            assert!(check("flagged.txt", &c, cutoff).is_none());
        }
    }

    // Check directory exclusion by name, suffix, prefix and user exclude list.
    #[test]
    fn test_dir_excluded_rules() {
        let c = Cfg { exclude: vec!["/work/skip".into()], ..cfg(Path::new("/")) };
        assert!(dir_excluded("/a/.ssh", ".ssh", &c));
        assert!(dir_excluded("/a/My.app", "My.app", &c));
        assert!(dir_excluded("/usr/lib", "lib", &c));
        assert!(dir_excluded("/work/skip/x", "x", &c));
        assert!(!dir_excluded("/work/ok", "ok", &c));
    }

    // Check each kind of directory entry is handled by scan_entry.
    #[test]
    fn test_scan_entry_kinds() {
        let sb = Sandbox::new();
        let root = sb.dir("root");
        let db = open_db(&sb.state(), true);
        let c = cfg(&root);
        let cutoff = util::now() + 100.0;
        let mut stats = Stats::default();
        sb.write("root/good.txt", &vec![b'a'; 1 << 20]);
        sb.write("root/small.txt", b"tiny");
        sb.dir("root/sub");
        sb.dir("root/.ssh");
        std::os::unix::fs::symlink(&root, root.join("link")).unwrap();
        let fifo = std::ffi::CString::new(root.join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: valid C path and mode.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        for name in ["good.txt", "small.txt", "sub", ".ssh", "link", "fifo"] {
            assert!(scan_entry(&db, &entry(&root, name), &c, cutoff, &mut stats).is_ok());
        }
        assert_eq!((stats.files, stats.cands), (2, 1));
        let dirs: i64 = db.query_row("SELECT COUNT(*) FROM dirs", [], |r| r.get(0)).unwrap();
        assert_eq!(dirs, 1);
        scan_entry(&db, &entry(&root, "good.txt"), &c, cutoff, &mut stats).unwrap();
        assert_eq!(stats.cands, 1);
        let other_dev = Cfg { dev: c.dev + 1, ..cfg(&root) };
        scan_entry(&db, &entry(&root, "sub"), &other_dev, cutoff, &mut stats).unwrap();
        let cross = Cfg { cross: true, ..other_dev };
        scan_entry(&db, &entry(&root, "sub"), &cross, cutoff, &mut stats).unwrap();
        if write_non_utf8(&root) {
            let bad = fs::read_dir(&root)
                .unwrap()
                .flatten()
                .find(|e| e.file_name().as_bytes() == b"bad\xff")
                .unwrap();
            assert!(scan_entry(&db, &bad, &c, cutoff, &mut stats).is_err());
        }
        db.execute_batch("DROP TABLE dirs; DROP TABLE files; DROP TABLE cands").unwrap();
        assert!(scan_entry(&db, &entry(&root, "sub"), &c, cutoff, &mut stats).is_err());
        assert!(scan_entry(&db, &entry(&root, "small.txt"), &c, cutoff, &mut stats).is_err());
    }

    // Check a candidate insert failure after recording the file is reported.
    #[test]
    fn test_scan_entry_candidate_insert_fails() {
        let sb = Sandbox::new();
        let root = sb.dir("root");
        sb.write("root/good.txt", &vec![b'a'; 1 << 20]);
        let db = open_db(&sb.state(), true);
        db.execute_batch("DROP TABLE cands").unwrap();
        let mut stats = Stats::default();
        let result = scan_entry(&db, &entry(&root, "good.txt"), &cfg(&root), util::now() + 100.0, &mut stats);
        assert!(result.is_err());
    }

    // Check a full scan finds candidates, skips excluded paths and can run again over the same state.
    #[test]
    fn test_cmd_new_full_scan() {
        let sb = Sandbox::new();
        let root = sb.dir("root");
        sb.write("root/a/good.txt", &vec![b'a'; 1 << 20]);
        sb.write("root/skip/also.txt", &vec![b'a'; 1 << 20]);
        sb.write("root/b/tiny", b"x");
        let mut o = new_opts(&sb, &root);
        o.exclude = vec![sb.path("root/skip").to_string_lossy().into_owned()];
        cmd_new(&o);
        let db = open_db(&sb.state(), false);
        let cands: i64 = db.query_row("SELECT COUNT(*) FROM cands", [], |r| r.get(0)).unwrap();
        assert_eq!(cands, 1);
        assert!(finished(&db));
        drop(db);
        cmd_new(&o);
        cmd_status(&sb.state());
        cmd_list(&sb.state(), "pending", 10);
        cmd_list(&sb.state(), "done", 10);
        cmd_resume(&sb.state());
    }

    // Check new refuses bad roots and unfinished scans unless forced.
    #[test]
    fn test_cmd_new_guards() {
        let sb = Sandbox::new();
        let root = sb.dir("root");
        sb.write("root/f", b"x");
        let file = sb.write("afile", b"x");
        let bad = |o: NewOpts| std::panic::catch_unwind(|| cmd_new(&o)).is_err();
        assert!(bad(new_opts(&sb, &sb.path("missing"))));
        assert!(bad(new_opts(&sb, &file)));
        let mut o = new_opts(&sb, &root);
        util::set_stop(true);
        cmd_new(&o);
        assert!(bad(new_opts(&sb, &root)));
        o.force = true;
        util::set_stop(false);
        cmd_new(&o);
        assert!(finished(&open_db(&sb.state(), false)));
    }

    // Check resume continues a paused scan and rejects missing configuration.
    #[test]
    fn test_cmd_resume_cases() {
        let sb = Sandbox::new();
        let root = sb.dir("root");
        sb.write("root/f", &vec![b'a'; 1 << 20]);
        drop(open_db(&sb.state(), true));
        let state = sb.state();
        assert!(std::panic::catch_unwind(|| cmd_resume(&state)).is_err());
        util::set_stop(true);
        cmd_new(&new_opts(&sb, &root));
        cmd_status(&sb.state());
        util::set_stop(false);
        cmd_resume(&sb.state());
        assert!(finished(&open_db(&sb.state(), false)));
        cmd_resume(&sb.state());
    }

    // Check the scan loop commits on its interval, reports unreadable directories and shows tty progress.
    #[test]
    fn test_run_scan_commit_errors_and_tty() {
        let sb = Sandbox::new();
        let root = sb.dir("root");
        sb.write("root/a/f", &vec![b'a'; 1 << 20]);
        sb.dir("root/gone");
        let bad_name = write_non_utf8(&root) as i64;
        let db = open_db(&sb.state(), true);
        db.execute("INSERT INTO dirs(path) VALUES(?)", [root.to_str().unwrap()]).unwrap();
        db.execute("INSERT INTO dirs(path) VALUES(?)", [sb.path("nonexistent").to_str().unwrap()]).unwrap();
        util::set_tty(true);
        assert!(run_scan(&db, &cfg(&root), -1.0));
        let stats = Stats::from_json(&meta_get(&db, "stats").unwrap());
        assert_eq!((stats.dirs, stats.errors), (4, 1 + bad_name));
        util::set_tty(false);
        assert!(run_scan(&db, &cfg(&root), 2.0));
    }

    // Check unreadable directories are counted as scan errors.
    #[test]
    fn test_run_scan_unreadable_dir() {
        let sb = Sandbox::new();
        if is_root() {
            return;
        }
        let root = sb.dir("root");
        sb.dir("root/locked");
        chmod(&sb.path("root/locked"), 0o000);
        let db = open_db(&sb.state(), true);
        db.execute("INSERT INTO dirs(path) VALUES(?)", [root.to_str().unwrap()]).unwrap();
        assert!(run_scan(&db, &cfg(&root), 2.0));
        assert_eq!(Stats::from_json(&meta_get(&db, "stats").unwrap()).errors, 1);
        chmod(&sb.path("root/locked"), 0o755);
    }

    // Check status and list output for populated and empty state files.
    #[test]
    fn test_status_and_list_output() {
        let sb = Sandbox::new();
        let db = open_db(&sb.state(), true);
        cmd_status(&sb.state());
        cmd_list(&sb.state(), "pending", 5);
        meta_set(&db, "cfg", &cfg(&sb.root).to_json());
        db.execute_batch(
            "INSERT INTO cands(path,size,est_saved,status,note,saved) VALUES
              ('/a',100,50,'pending',NULL,0),('/b',200,90,'pending','',0),
              ('/c',300,80,'done','ok note',70),('/d',10,5,'failed',NULL,0);
             INSERT INTO files(path,name,size,cloned) VALUES ('/a','a',1,1),('/b','b',1,0);
             INSERT INTO dirs(path) VALUES ('/x');",
        )
        .unwrap();
        drop(db);
        cmd_status(&sb.state());
        cmd_list(&sb.state(), "pending", 5);
        cmd_list(&sb.state(), "done", 1);
    }
}
