// Low-level file access: identity stamps, whole-file loading, content hashing and binary sniffing.
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const RACY_NS: i128 = 2_000_000_000;
const SNIFF_BYTES: usize = 8000;
const TRIAL_FILES: u64 = 128;

// A file's stamp and bytes, or the error that stopped loading it.
pub type Loaded = io::Result<(Ident, Vec<u8>)>;

// Cheap stamp that proves a file unchanged when it matches and is old enough to trust.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ident {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
}

impl Ident {
    // Build the stamp from file metadata.
    pub fn of(md: &Metadata) -> Ident {
        let ns = |s: i64, n: i64| s as i128 * 1_000_000_000 + n as i128;
        Ident {
            dev: md.dev(),
            ino: md.ino(),
            size: md.size(),
            mtime_ns: ns(md.mtime(), md.mtime_nsec()),
            ctime_ns: ns(md.ctime(), md.ctime_nsec()),
        }
    }

    // Build a stamp from raw file attributes, for loaders that do not go through std metadata.
    #[cfg(target_os = "linux")]
    pub fn from_parts(dev: u64, ino: u64, size: u64, mtime_ns: i128, ctime_ns: i128) -> Ident {
        Ident { dev, ino, size, mtime_ns, ctime_ns }
    }

    // Return true when the stamp is too recent to prove anything at the given time.
    pub fn racy(&self, at_ns: i128) -> bool {
        self.mtime_ns.max(self.ctime_ns) + RACY_NS > at_ns
    }
}

// Return the current time in nanoseconds since the Unix epoch.
pub fn now_ns() -> i128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0)
}

// Return the stamp of a path without following symlinks, or None when it cannot be read.
pub fn ident_of(path: &Path) -> Option<Ident> {
    std::fs::symlink_metadata(path).ok().map(|m| Ident::of(&m))
}

// Open a file read-only, skipping access-time updates where the OS allows it.
#[cfg(target_os = "linux")]
fn open(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    retry_without_noatime(
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOATIME).open(path),
        path,
    )
}

// Reopen a file normally when the O_NOATIME open was refused because the caller does not own it.
#[cfg(target_os = "linux")]
fn retry_without_noatime(first: io::Result<File>, path: &Path) -> io::Result<File> {
    match first {
        Err(e) if e.raw_os_error() == Some(libc::EPERM) => File::open(path),
        other => other,
    }
}

// Open a file read-only.
#[cfg(not(target_os = "linux"))]
fn open(path: &Path) -> io::Result<File> {
    File::open(path)
}

// Tell the kernel the file will be read once from start to end.
#[cfg(target_os = "linux")]
fn advise_sequential(file: &File) {
    use std::os::unix::io::AsRawFd;
    // SAFETY: the descriptor is valid for the lifetime of `file`; the call only hints the kernel.
    unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_SEQUENTIAL) };
}

// Do nothing where no sequential-read hint exists.
#[cfg(not(target_os = "linux"))]
fn advise_sequential(_file: &File) {}

// Read a whole regular file and return its stamp with its bytes.
pub fn load(path: &Path) -> io::Result<(Ident, Vec<u8>)> {
    let mut file = open(path)?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    advise_sequential(&file);
    let mut buf = Vec::with_capacity(md.len() as usize + 1);
    file.read_to_end(&mut buf)?;
    Ok((Ident::of(&md), buf))
}

// How batched loading is chosen: by timing both methods, or forced either way by AFS_BATCH_IO.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Always,
    Never,
}

// Read the AFS_BATCH_IO setting once.
fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| parse_mode(std::env::var("AFS_BATCH_IO").ok().as_deref()))
}

// Map the AFS_BATCH_IO value to a mode; anything but always or never means auto.
fn parse_mode(value: Option<&str>) -> Mode {
    match value {
        Some("always") => Mode::Always,
        Some("never") => Mode::Never,
        _ => Mode::Auto,
    }
}

// Files and nanoseconds measured for batched loading, then for plain loading.
static TRIAL: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

// Decide whether the next call should use batched loading: alternate during the trial, then pick the faster.
fn prefer_batch() -> bool {
    decide(mode(), [0, 1, 2, 3].map(|i| TRIAL[i].load(Ordering::Relaxed)))
}

// Choose batching from the mode and the trial totals [batch files, batch ns, plain files, plain ns].
fn decide(mode: Mode, totals: [u64; 4]) -> bool {
    let [batch_files, batch_ns, plain_files, plain_ns] = totals;
    match mode {
        Mode::Never => false,
        Mode::Always => true,
        Mode::Auto if batch_files < TRIAL_FILES && batch_files <= plain_files => true,
        Mode::Auto if plain_files < TRIAL_FILES => false,
        Mode::Auto => {
            u128::from(batch_ns) * u128::from(plain_files) <= u128::from(plain_ns) * u128::from(batch_files)
        }
    }
}

// Add a measurement for one method while its trial is still running.
fn record_trial(batched: bool, files: usize, started: Instant) {
    let slot = if batched { 0 } else { 2 };
    if TRIAL[slot].load(Ordering::Relaxed) < TRIAL_FILES {
        TRIAL[slot].fetch_add(files as u64, Ordering::Relaxed);
        TRIAL[slot + 1].fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

// Load files with the platform's batched I/O, or None when it does not exist or is unusable here.
fn load_batched(paths: &[&Path]) -> Option<Vec<Loaded>> {
    #[cfg(target_os = "linux")]
    return crate::uring::load_many(paths);
    #[cfg(target_os = "macos")]
    return Some(crate::aio::load_many(paths));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return {
        let _ = paths;
        None
    };
}

// Load many files with io_uring (Linux) or POSIX AIO (macOS) when that is faster than plain reads here.
pub fn load_many(paths: &[&Path]) -> Vec<Loaded> {
    load_with(paths, prefer_batch(), load_batched)
}

// Load with the batched loader when preferred and available, else one file at a time, recording the timing.
fn load_with(
    paths: &[&Path],
    batch: bool,
    batched: impl FnOnce(&[&Path]) -> Option<Vec<Loaded>>,
) -> Vec<Loaded> {
    let started = Instant::now();
    if let Some(done) = batch.then(|| batched(paths)).flatten() {
        record_trial(true, paths.len(), started);
        return done;
    }
    let done = paths.iter().map(|p| load(p)).collect();
    record_trial(false, paths.len(), started);
    done
}

// Return a short content hash used as an etag.
pub fn etag(bytes: &[u8]) -> String {
    let hash = blake3::hash(bytes);
    format!("b3:{}", &hash.to_hex()[..16])
}

// Return true when the first bytes contain a NUL, which marks binary data.
pub fn is_binary(bytes: &[u8]) -> bool {
    memchr::memchr(0, &bytes[..bytes.len().min(SNIFF_BYTES)]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Equal content gives equal etags and different content differs.
    #[test]
    fn test_etag_content_sensitive() {
        assert_eq!(etag(b"a"), etag(b"a"));
        assert_ne!(etag(b"a"), etag(b"b"));
    }

    // Batched loading returns exactly what one-by-one loading returns, including errors.
    #[test]
    fn test_load_many_matches_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = vec![];
        for (name, len) in
            [("empty", 0usize), ("small", 10), ("edge", 65536), ("over", 65537), ("big", 300_000)]
        {
            let p = dir.path().join(name);
            std::fs::write(&p, (0..len).map(|i| (i % 251) as u8).collect::<Vec<u8>>()).unwrap();
            paths.push(p);
        }
        paths.push(dir.path().join("missing"));
        paths.push(dir.path().to_path_buf());
        let refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        let many = load_many(&refs);
        assert_eq!(many.len(), refs.len());
        for (p, got) in refs.iter().zip(many) {
            match (load(p), got) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "{p:?}"),
                (Err(_), Err(_)) => {}
                (a, b) => panic!("{p:?}: {a:?} vs {b:?}"),
            }
        }
    }

    // Many more files than one batch still load in order.
    #[test]
    fn test_load_many_large_batch() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..200)
            .map(|i| {
                let p = dir.path().join(format!("f{i}"));
                std::fs::write(&p, format!("content {i}")).unwrap();
                p
            })
            .collect();
        let refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        for (i, got) in load_many(&refs).into_iter().enumerate() {
            assert_eq!(got.unwrap().1, format!("content {i}").into_bytes());
        }
    }

    // A NUL in the head marks data binary and text is not binary.
    #[test]
    fn test_is_binary_nul() {
        assert!(is_binary(b"ab\0cd"));
        assert!(!is_binary(b"plain text\n"));
    }

    // A freshly written file is racy now and trusted once the window has passed.
    #[test]
    fn test_ident_racy_window() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, "x").unwrap();
        let id = ident_of(&p).unwrap();
        assert!(id.racy(now_ns()));
        assert!(!id.racy(now_ns() + 10_000_000_000));
    }

    // An EPERM from the no-atime open is retried normally and other results pass through.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_retry_without_noatime() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, "x").unwrap();
        assert!(retry_without_noatime(Err(io::Error::from_raw_os_error(libc::EPERM)), &f).is_ok());
        let other = retry_without_noatime(Err(io::Error::from_raw_os_error(libc::ENOENT)), &f);
        assert_eq!(other.unwrap_err().raw_os_error(), Some(libc::ENOENT));
        assert!(retry_without_noatime(File::open(&f), &f).is_ok());
    }

    // The environment value maps to a mode.
    #[test]
    fn test_parse_mode_values() {
        assert!(parse_mode(Some("always")) == Mode::Always);
        assert!(parse_mode(Some("never")) == Mode::Never);
        assert!(parse_mode(Some("other")) == Mode::Auto);
        assert!(parse_mode(None) == Mode::Auto);
    }

    // Auto mode alternates during the trial, then picks the faster method.
    #[test]
    fn test_decide_modes() {
        assert!(!decide(Mode::Never, [0; 4]));
        assert!(decide(Mode::Always, [0; 4]));
        assert!(decide(Mode::Auto, [0, 0, 0, 0]));
        assert!(!decide(Mode::Auto, [10, 5, 5, 5]));
        let done = TRIAL_FILES;
        assert!(decide(Mode::Auto, [done, 100, done, 200]));
        assert!(!decide(Mode::Auto, [done, 300, done, 200]));
        assert!(!decide(Mode::Auto, [done, 1, 3, 1]));
    }

    // The batched loader is used when preferred and present, and plain loading covers every other case.
    #[test]
    fn test_load_with_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, "abc").unwrap();
        let paths = [f.as_path()];
        let batched = load_with(&paths, true, |_| Some(vec![Err(io::Error::other("marker"))]));
        assert_eq!(batched[0].as_ref().unwrap_err().to_string(), "marker");
        let unavailable = load_with(&paths, true, |_| None);
        assert_eq!(unavailable[0].as_ref().unwrap().1, b"abc");
        let skipped = load_with(&paths, false, |_| panic!("batched loader must not run"));
        assert_eq!(skipped[0].as_ref().unwrap().1, b"abc");
    }
}
