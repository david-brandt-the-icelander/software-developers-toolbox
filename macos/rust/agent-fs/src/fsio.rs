// Low-level file access: identity stamps, whole-file loading, content hashing and binary sniffing.
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const RACY_NS: i128 = 2_000_000_000;
const SNIFF_BYTES: usize = 8000;

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
    match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOATIME).open(path) {
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
}
