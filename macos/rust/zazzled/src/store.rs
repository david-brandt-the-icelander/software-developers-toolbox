// The persistent cache: an append-only log of per-path records under ~/.zazzle, derived data only.
use crate::meta::Info;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAGIC: &[u8; 8] = b"ZZLPATH1";
const COMPACT_MIN_RECORDS: usize = 1000;
const RACY_NS: i64 = 2_000_000_000;
const F_BINARY: u8 = 1;
const F_MINIFIED: u8 = 2;
const F_CRLF: u8 = 4;
const F_RACY: u8 = 8;

// What stat says about a file; a record is valid only while this is unchanged.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StatKey {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
}

// A stored record: the stat identity, the derived info and whether it was taken too close to the mtime to trust.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Rec {
    key: StatKey,
    info: Info,
    racy: bool,
}

// Mutable store state behind one lock.
struct Inner {
    map: HashMap<PathBuf, Rec>,
    pending: Vec<u8>,
    file: File,
    records: usize,
}

// The cache directory handle.
pub struct Store {
    dir: PathBuf,
    inner: Mutex<Inner>,
}

// Hashes bytes with FNV-1a for record checksums.
fn fnv32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(*b)).wrapping_mul(0x0100_0193))
}

// Encodes one record, including its length prefix and checksum.
fn encode(path: &Path, rec: &Rec) -> Vec<u8> {
    let p = path.as_os_str().as_encoded_bytes();
    let mut body = Vec::with_capacity(p.len() + 80);
    body.extend_from_slice(&(p.len() as u16).to_le_bytes());
    body.extend_from_slice(p);
    for v in [rec.key.dev, rec.key.ino, rec.key.size] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    body.extend_from_slice(&rec.key.mtime_ns.to_le_bytes());
    body.extend_from_slice(&rec.info.hash);
    body.extend_from_slice(&rec.info.lines.to_le_bytes());
    let flags = [
        (rec.info.binary, F_BINARY),
        (rec.info.minified, F_MINIFIED),
        (rec.info.crlf, F_CRLF),
        (rec.racy, F_RACY),
    ];
    body.push(flags.iter().filter(|(on, _)| *on).fold(0, |acc, (_, bit)| acc | bit));
    let mut out = ((body.len() + 4) as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&fnv32(&body).to_le_bytes());
    out
}

// Reads a little-endian integer of N bytes from the front of a slice, advancing it.
fn take<const N: usize>(buf: &mut &[u8]) -> Option<[u8; N]> {
    let (head, rest) = buf.split_first_chunk::<N>()?;
    *buf = rest;
    Some(*head)
}

// Decodes one record body (without length prefix and checksum).
fn decode_body(mut b: &[u8]) -> Option<(PathBuf, Rec)> {
    let plen = u16::from_le_bytes(take::<2>(&mut b)?) as usize;
    let (path, mut b) = b.split_at_checked(plen)?;
    let [dev, ino, size] = [(); 3].map(|()| take::<8>(&mut b).map(u64::from_le_bytes));
    let mtime_ns = i64::from_le_bytes(take::<8>(&mut b)?);
    let hash = take::<32>(&mut b)?;
    let lines = u32::from_le_bytes(take::<4>(&mut b)?);
    let [flags] = take::<1>(&mut b)?;
    let key = StatKey { dev: dev?, ino: ino?, size: size?, mtime_ns };
    let info = Info {
        size: key.size,
        hash,
        binary: flags & F_BINARY != 0,
        minified: flags & F_MINIFIED != 0,
        crlf: flags & F_CRLF != 0,
        lines,
    };
    let path = PathBuf::from(os_string(path));
    Some((path, Rec { key, info, racy: flags & F_RACY != 0 }))
}

// Rebuilds an OS string from bytes written by encode.
fn os_string(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes.to_vec())
}

// Parses a log, returning the records in order and the length of the valid prefix.
fn parse_log(data: &[u8]) -> (Vec<(PathBuf, Rec)>, usize) {
    if data.len() < MAGIC.len() || &data[..MAGIC.len()] != MAGIC {
        return (vec![], 0);
    }
    let (mut at, mut out) = (MAGIC.len(), Vec::new());
    while let Some(len) = data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    {
        let Some(chunk) = data.get(at + 4..at + 4 + len).filter(|c| c.len() >= 4) else { break };
        let (body, sum) = chunk.split_at(chunk.len() - 4);
        if fnv32(body).to_le_bytes() != sum {
            break;
        }
        let Some(rec) = decode_body(body) else { break };
        out.push(rec);
        at += 4 + len;
    }
    (out, at)
}

// Returns the current time in nanoseconds since the epoch.
fn now_ns() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i64)
}

// Creates the directory with owner-only permissions and keeps Spotlight and backups away from it.
fn prepare_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let fresh = !dir.exists();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    if fresh {
        std::fs::write(dir.join(".metadata_never_index"), b"")?;
        #[cfg(target_os = "macos")]
        let _ = crate::launchd::run_with_timeout(
            "tmutil",
            &["addexclusion", &dir.to_string_lossy()],
            std::time::Duration::from_secs(5),
        );
    }
    Ok(())
}

impl Store {
    // Opens (creating if needed) the cache in a directory, repairing a torn tail and compacting garbage.
    pub fn open(dir: &Path) -> io::Result<Store> {
        prepare_dir(dir)?;
        let path = dir.join("paths.log");
        let mut data = Vec::new();
        if path.exists() {
            File::open(&path)?.read_to_end(&mut data)?;
        }
        let (recs, valid) = parse_log(&data);
        let records = recs.len();
        let map: HashMap<PathBuf, Rec> = recs.into_iter().collect();
        let needs_rewrite = valid == 0 || (records >= COMPACT_MIN_RECORDS && records > 2 * map.len());
        if needs_rewrite {
            rewrite(&path, &map)?;
        } else if valid < data.len() {
            OpenOptions::new().write(true).open(&path)?.set_len(valid as u64)?;
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        let records = if needs_rewrite { map.len() } else { records };
        Ok(Store {
            dir: dir.to_path_buf(),
            inner: Mutex::new(Inner { map, pending: Vec::new(), file, records }),
        })
    }

    // Returns the stored info for a path if its stat still matches and the record is trustworthy.
    pub fn get(&self, path: &Path, key: &StatKey) -> Option<Info> {
        let inner = self.inner.lock().expect("store lock");
        inner.map.get(path).filter(|r| r.key == *key && !r.racy).map(|r| r.info)
    }

    // Records info for a path, skipping the write when the stored record already matches.
    pub fn put(&self, path: &Path, key: StatKey, info: Info) {
        let racy = now_ns() - key.mtime_ns < RACY_NS;
        let rec = Rec { key, info, racy };
        let mut inner = self.inner.lock().expect("store lock");
        if inner.map.get(path) == Some(&rec) {
            return;
        }
        inner.pending.extend(encode(path, &rec));
        inner.records += 1;
        inner.map.insert(path.to_path_buf(), rec);
    }

    // Writes pending records to disk; returns whether anything was written.
    pub fn flush(&self) -> io::Result<bool> {
        let mut inner = self.inner.lock().expect("store lock");
        if inner.pending.is_empty() {
            return Ok(false);
        }
        let bytes = std::mem::take(&mut inner.pending);
        inner.file.write_all(&bytes)?;
        Ok(true)
    }

    // Returns the number of live records.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("store lock").map.len()
    }

    // Returns true if there are no live records.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Returns the cache directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // Swaps the log handle for a read-only one so the next flush fails, for tests.
    #[cfg(test)]
    pub fn break_log(&self) {
        let ro = File::open(self.dir.join("paths.log")).expect("open log read-only");
        self.inner.lock().expect("store lock").file = ro;
    }
}

// Rewrites the log atomically with only the live records.
fn rewrite(path: &Path, map: &HashMap<PathBuf, Rec>) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut out = MAGIC.to_vec();
    for (p, r) in map {
        out.extend(encode(p, r));
    }
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)
}

// Returns the default cache directory: ZAZZLE_CACHE_DIR or ~/.zazzle/v1.
pub fn default_dir() -> io::Result<PathBuf> {
    crate::util::home_path(std::env::var_os("ZAZZLE_CACHE_DIR"), std::env::var_os("HOME"), ".zazzle/v1")
        .map_err(io::Error::other)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Builds a stat key.
    fn key(size: u64) -> StatKey {
        StatKey { dev: 1, ino: 2, size, mtime_ns: 1_000_000_000 }
    }

    // Builds an info record.
    fn info(lines: u32) -> Info {
        Info { size: 10, hash: [7; 32], binary: false, minified: false, crlf: false, lines }
    }

    // Checks every field survives encode and decode, including flags and a non-UTF-8 path.
    #[test]
    fn test_record_roundtrip() {
        use std::os::unix::ffi::OsStrExt;
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/a/\xff/b.rs"));
        let rec = Rec {
            key: key(10),
            info: Info { binary: true, minified: true, crlf: true, ..info(5) },
            racy: true,
        };
        let bytes = encode(&path, &rec);
        let (recs, valid) = parse_log(&[MAGIC.as_slice(), &bytes].concat());
        assert_eq!(valid, MAGIC.len() + bytes.len());
        assert_eq!(recs, vec![(path, rec)]);
    }

    // Checks bad headers, truncation and checksum failures stop parsing at the last good record.
    #[test]
    fn test_parse_log_stops_at_damage() {
        let a = encode(Path::new("/a"), &Rec { key: key(1), info: info(1), racy: false });
        let b = encode(Path::new("/b"), &Rec { key: key(2), info: info(2), racy: false });
        let good = [MAGIC.as_slice(), &a, &b].concat();
        assert_eq!(parse_log(&good).0.len(), 2);
        assert_eq!(parse_log(b"nope").1, 0);
        assert_eq!(parse_log(b"").1, 0);
        assert_eq!(parse_log(&good[..good.len() - 3]).0.len(), 1);
        let mut flipped = good.clone();
        let n = flipped.len();
        flipped[n - 6] ^= 0xff;
        assert_eq!(parse_log(&flipped).0.len(), 1);
        let mut tiny = MAGIC.to_vec();
        tiny.extend_from_slice(&2u32.to_le_bytes());
        tiny.extend_from_slice(&[0, 0]);
        assert_eq!(parse_log(&tiny).0.len(), 0);
        let mut bogus = MAGIC.to_vec();
        let body = [9u8, 9, 9, 9];
        bogus.extend_from_slice(&8u32.to_le_bytes());
        bogus.extend_from_slice(&body);
        bogus.extend_from_slice(&fnv32(&body).to_le_bytes());
        assert_eq!(parse_log(&bogus).0.len(), 0);
    }

    // Checks decode rejects bodies that are too short for their fields.
    #[test]
    fn test_decode_body_short_inputs() {
        assert!(decode_body(&[]).is_none());
        assert!(decode_body(&[5, 0, b'a']).is_none());
        let rec = Rec { key: key(1), info: info(1), racy: false };
        let full = encode(Path::new("/a"), &rec);
        let body = &full[4..full.len() - 4];
        for cut in [3usize, 10, 20, 40, 60, body.len() - 1] {
            assert!(decode_body(&body[..cut]).is_none(), "cut {cut}");
        }
    }

    // Checks put, flush, reopen and get across restarts.
    #[test]
    fn test_persists_across_open() {
        let d = TempDir::new();
        let dir = d.path().join("cache");
        {
            let s = Store::open(&dir).expect("open");
            assert!(s.is_empty());
            s.put(Path::new("/f"), key(10), info(3));
            assert_eq!(s.len(), 1);
            assert!(s.flush().expect("flush"));
            assert!(!s.flush().expect("flush again"));
            assert_eq!(s.dir(), dir);
        }
        let s = Store::open(&dir).expect("reopen");
        assert_eq!(s.get(Path::new("/f"), &key(10)).map(|i| i.lines), Some(3));
        assert!(s.get(Path::new("/f"), &key(11)).is_none());
        assert!(s.get(Path::new("/other"), &key(10)).is_none());
    }

    // Checks identical puts do not grow the log and newer records win.
    #[test]
    fn test_put_dedupes_and_overrides() {
        let d = TempDir::new();
        let s = Store::open(d.path()).expect("open");
        s.put(Path::new("/f"), key(10), info(3));
        s.put(Path::new("/f"), key(10), info(3));
        assert_eq!(s.inner.lock().expect("lock").records, 1);
        s.put(Path::new("/f"), key(10), info(4));
        assert_eq!(s.get(Path::new("/f"), &key(10)).map(|i| i.lines), Some(4));
    }

    // Checks records taken close to the mtime are not served.
    #[test]
    fn test_racy_records_are_not_served() {
        let d = TempDir::new();
        let s = Store::open(d.path()).expect("open");
        let fresh = StatKey { mtime_ns: now_ns(), ..key(10) };
        s.put(Path::new("/f"), fresh, info(1));
        assert!(s.get(Path::new("/f"), &fresh).is_none());
    }

    // Checks a torn tail is trimmed on open and the store keeps working.
    #[test]
    fn test_torn_tail_is_repaired() {
        let d = TempDir::new();
        {
            let s = Store::open(d.path()).expect("open");
            s.put(Path::new("/a"), key(1), info(1));
            s.put(Path::new("/b"), key(2), info(2));
            s.flush().expect("flush");
        }
        let log = d.path().join("paths.log");
        let len = std::fs::metadata(&log).expect("meta").len();
        OpenOptions::new().write(true).open(&log).expect("open").set_len(len - 5).expect("truncate");
        let s = Store::open(d.path()).expect("reopen");
        assert_eq!(s.len(), 1);
        s.put(Path::new("/c"), key(3), info(3));
        s.flush().expect("flush");
        assert_eq!(Store::open(d.path()).expect("third open").len(), 2);
    }

    // Checks a log with the wrong header is replaced.
    #[test]
    fn test_wrong_header_is_rebuilt() {
        let d = TempDir::new();
        d.write("paths.log", b"garbage that is not a log");
        let s = Store::open(d.path()).expect("open");
        assert!(s.is_empty());
        s.put(Path::new("/a"), key(1), info(1));
        s.flush().expect("flush");
        assert_eq!(Store::open(d.path()).expect("reopen").len(), 1);
    }

    // Checks compaction drops overwritten records when garbage dominates.
    #[test]
    fn test_compaction_on_open() {
        let d = TempDir::new();
        {
            let s = Store::open(d.path()).expect("open");
            for i in 0..(COMPACT_MIN_RECORDS + 50) as u32 {
                s.put(Path::new("/same"), key(10), info(i));
            }
            s.flush().expect("flush");
        }
        let before = std::fs::metadata(d.path().join("paths.log")).expect("meta").len();
        let s = Store::open(d.path()).expect("compacting open");
        let after = std::fs::metadata(d.path().join("paths.log")).expect("meta").len();
        assert!(after < before / 100);
        assert_eq!(s.len(), 1);
        assert_eq!(s.inner.lock().expect("lock").records, 1);
    }

    // Checks the directory is private and marked for Spotlight exclusion.
    #[test]
    fn test_directory_is_private_and_unindexed() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new();
        let dir = d.path().join("x/y");
        Store::open(&dir).expect("open");
        assert_eq!(std::fs::metadata(&dir).expect("meta").permissions().mode() & 0o077, 0);
        assert!(dir.join(".metadata_never_index").exists());
        Store::open(&dir).expect("reopen existing");
    }

    // Checks open fails cleanly when the path is a file.
    #[test]
    fn test_open_failure() {
        let d = TempDir::new();
        let f = d.write("f", b"x");
        assert!(Store::open(&f).is_err());
    }

    // Checks the default directory comes from the environment.
    #[test]
    fn test_default_dir() {
        let _g = crate::testutil::env_lock();
        std::env::set_var("ZAZZLE_CACHE_DIR", "/x/cache");
        assert_eq!(default_dir().expect("dir"), PathBuf::from("/x/cache"));
        std::env::remove_var("ZAZZLE_CACHE_DIR");
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set for tests"));
        assert_eq!(default_dir().expect("dir"), home.join(".zazzle/v1"));
    }

    // Checks concurrent puts and gets do not lose records.
    #[test]
    fn test_concurrent_access() {
        let d = TempDir::new();
        let s = Store::open(d.path()).expect("open");
        std::thread::scope(|sc| {
            for t in 0..4u64 {
                let s = &s;
                sc.spawn(move || {
                    for i in 0..100u64 {
                        s.put(Path::new(&format!("/t{t}/f{i}")), key(i), info(1));
                        let _ = s.get(Path::new("/t0/f0"), &key(0));
                    }
                });
            }
        });
        assert_eq!(s.len(), 400);
    }
}
