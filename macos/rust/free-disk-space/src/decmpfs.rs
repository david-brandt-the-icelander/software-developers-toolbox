// Writes APFS transparent compression (decmpfs zlib in the resource fork), since `ditto --hfsCompression` no longer compresses.
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use flate2::write::ZlibEncoder;
use flate2::Compression;

const BLOCK: usize = 64 * 1024;
const DATA_OFFSET: usize = 0x100;
#[cfg(any(target_os = "macos", test))]
const ZLIB_RSRC_TYPE: u32 = 4;
// Resource map naming a single 'cmpf' resource, as the kernel expects.
const RSRC_MAP: [u8; 50] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1C, 0, 0x32, 0, 0, b'c',
    b'm', b'p', b'f', 0, 0, 0, 0x0A, 0, 1, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0,
];

// Compress one 64 KiB block with zlib, or store it raw behind a 0xFF marker when zlib does not help.
fn compress_block(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(6));
    enc.write_all(data)?;
    let out = enc.finish()?;
    if out.len() < data.len() {
        return Ok(out);
    }
    let mut raw = Vec::with_capacity(data.len() + 1);
    raw.push(0xFF);
    raw.extend_from_slice(data);
    Ok(raw)
}

// Build the resource fork holding the file's compressed blocks.
pub fn build_fork(mut src: impl Read) -> std::io::Result<Vec<u8>> {
    let mut blocks = Vec::new();
    let mut buf = vec![0u8; BLOCK];
    loop {
        let mut n = 0;
        while n < BLOCK {
            let got = src.read(&mut buf[n..])?;
            if got == 0 {
                break;
            }
            n += got;
        }
        if n == 0 {
            break;
        }
        blocks.push(compress_block(&buf[..n])?);
        if n < BLOCK {
            break;
        }
    }
    let table = 4 + 8 * blocks.len();
    let payload = table + blocks.iter().map(Vec::len).sum::<usize>();
    let map_offset = DATA_OFFSET + 4 + payload;
    let too_big = || std::io::Error::other("compressed data exceeds 4 GiB");
    let be = |v: usize| u32::try_from(v).map(u32::to_be_bytes).map_err(|_| too_big());
    let le = |v: usize| u32::try_from(v).map(u32::to_le_bytes).map_err(|_| too_big());
    let mut fork = Vec::with_capacity(map_offset + RSRC_MAP.len());
    fork.extend(be(DATA_OFFSET)?);
    fork.extend(be(map_offset)?);
    fork.extend(be(map_offset - DATA_OFFSET)?);
    fork.extend(be(RSRC_MAP.len())?);
    fork.resize(DATA_OFFSET, 0);
    fork.extend(be(payload)?);
    fork.extend(le(blocks.len())?);
    let mut offset = table;
    for b in &blocks {
        fork.extend(le(offset)?);
        fork.extend(le(b.len())?);
        offset += b.len();
    }
    for b in &blocks {
        fork.extend_from_slice(b);
    }
    fork.extend_from_slice(&RSRC_MAP);
    Ok(fork)
}

// Build the 16-byte com.apple.decmpfs header for a zlib resource-fork file.
#[cfg(any(target_os = "macos", test))]
fn header(size: u64) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[..4].copy_from_slice(b"fpmc");
    h[4..8].copy_from_slice(&ZLIB_RSRC_TYPE.to_le_bytes());
    h[8..].copy_from_slice(&size.to_le_bytes());
    h
}

// Wrap the last OS error with the step that failed.
#[cfg(target_os = "macos")]
fn step_error(step: &str) -> std::io::Error {
    let e = std::io::Error::last_os_error();
    std::io::Error::new(e.kind(), format!("{step}: {e}"))
}

// Set one extended attribute through an open file descriptor.
#[cfg(target_os = "macos")]
fn set_xattr(fd: libc::c_int, name: &str, value: &[u8]) -> std::io::Result<()> {
    let name = std::ffi::CString::new(name).map_err(std::io::Error::other)?;
    // SAFETY: fd is open, name is NUL-terminated and value is a valid slice for its length.
    let rc = unsafe { libc::fsetxattr(fd, name.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0) };
    if rc != 0 {
        return Err(step_error(&format!("setxattr {}", name.to_string_lossy())));
    }
    Ok(())
}

// Turn a plain file into a transparently compressed one holding the given fork.
#[cfg(target_os = "macos")]
pub fn apply(path: &Path, size: u64, fork: &[u8]) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let file = fs::OpenOptions::new().write(true).open(path)?;
    let fd = file.as_raw_fd();
    set_xattr(fd, "com.apple.ResourceFork", fork)?;
    set_xattr(fd, "com.apple.decmpfs", &header(size))?;
    file.set_len(0)?;
    let flags = crate::util::flags(&file.metadata()?);
    // SAFETY: fd is an open file descriptor owned by `file`.
    if unsafe { libc::fchflags(fd, flags | crate::util::UF_COMPRESSED) } != 0 {
        return Err(step_error("chflags"));
    }
    Ok(())
}

// Leave the file unchanged where APFS compression does not exist; tests drive the flags instead.
#[cfg(not(target_os = "macos"))]
pub fn apply(path: &Path, _size: u64, _fork: &[u8]) -> std::io::Result<()> {
    fs::symlink_metadata(path).map(|_| ())
}

// Unit tests for the decmpfs layout.
#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::ZlibDecoder;

    // Decode a fork the way the kernel does and return the original bytes.
    fn decode(fork: &[u8]) -> Vec<u8> {
        let be = |at: usize| u32::from_be_bytes(fork[at..at + 4].try_into().unwrap()) as usize;
        let le = |at: usize| u32::from_le_bytes(fork[at..at + 4].try_into().unwrap()) as usize;
        assert_eq!(be(0), DATA_OFFSET);
        assert_eq!(be(4) + be(12), fork.len());
        assert_eq!(be(4) - be(0), be(8));
        assert_eq!(be(DATA_OFFSET) + DATA_OFFSET + 4, be(4));
        assert_eq!(&fork[be(4)..], &RSRC_MAP);
        let base = DATA_OFFSET + 4;
        let mut out = Vec::new();
        for i in 0..le(base) {
            let (off, len) = (le(base + 4 + 8 * i), le(base + 8 + 8 * i));
            let block = &fork[base + off..base + off + len];
            if block[0] & 0x0F == 0x0F {
                out.extend_from_slice(&block[1..]);
            } else {
                ZlibDecoder::new(block).read_to_end(&mut out).unwrap();
            }
        }
        out
    }

    // Check compressible, incompressible, multi-block and empty input round-trip through the fork.
    #[test]
    fn test_build_fork_round_trip() {
        let text: Vec<u8> =
            b"hello compressible line\n".iter().copied().cycle().take(3 * BLOCK + 17).collect();
        let fork = build_fork(&text[..]).unwrap();
        assert!(fork.len() < text.len() / 10);
        assert_eq!(decode(&fork), text);
        let mut x: u32 = 1;
        let noise: Vec<u8> = (0..BLOCK + 5)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        assert_eq!(decode(&build_fork(&noise[..]).unwrap()), noise);
        let exact = vec![7u8; BLOCK];
        assert_eq!(decode(&build_fork(&exact[..]).unwrap()), exact);
        assert!(decode(&build_fork(&[][..]).unwrap()).is_empty());
    }

    // Check the decmpfs header layout.
    #[test]
    fn test_header_layout() {
        let h = header(0x0102_0304_0506);
        assert_eq!(&h[..4], b"fpmc");
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 4);
        assert_eq!(u64::from_le_bytes(h[8..].try_into().unwrap()), 0x0102_0304_0506);
    }

    // Check a real file compresses transparently and reads back unchanged on macOS.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_apply_real_file() {
        let sb = crate::testkit::Sandbox::new();
        let text: Vec<u8> =
            b"hello compressible line\n".iter().copied().cycle().take(5 * BLOCK + 3).collect();
        let path = sb.write("c.txt", &text);
        let fork = build_fork(&text[..]).unwrap();
        apply(&path, text.len() as u64, &fork).unwrap();
        let md = fs::symlink_metadata(&path).unwrap();
        assert_ne!(crate::util::flags(&md) & crate::util::UF_COMPRESSED, 0);
        assert_eq!(md.len(), text.len() as u64);
        assert!(crate::util::alloc(&md) < text.len() as i64 / 4);
        assert_eq!(fs::read(&path).unwrap(), text);
    }

    // Check applying to a missing file fails.
    #[test]
    fn test_apply_missing_file() {
        assert!(apply(Path::new("/nonexistent/fds"), 1, &[]).is_err());
    }
}
