// Safe wrapper over the vendored lzfse-zazzcat C library: compress a file into APFS transparent compression, and
// check the compressed form decodes back to the file with Apple's decoder.
use std::ffi::{c_char, c_int, c_void, CStr};
use std::fs::File;
use std::os::unix::io::AsRawFd;

// The library's growable byte buffer (lzfse_zazzcat_buffer).
#[repr(C)]
struct Buffer {
    data: *mut u8,
    length: usize,
    capacity: usize,
}

// The two attributes of a compressed file (lzfse_zazzcat_decmpfs).
#[repr(C)]
struct Decmpfs {
    xattr: Buffer,
    fork: Buffer,
}

extern "C" {
    fn lzfse_zazzcat_decmpfs_compress(data: *const c_void, length: usize, result: *mut Decmpfs) -> c_int;
    fn lzfse_zazzcat_decmpfs_free(compressed: *mut Decmpfs);
    fn lzfse_zazzcat_decmpfs_apply(
        fd: c_int,
        compressed: *const Decmpfs,
        failed_step: *mut *const c_char,
    ) -> c_int;
    fn lzfse_zazzcat_status_string(status: c_int) -> *const c_char;
    #[cfg(not(target_os = "macos"))]
    fn lzfse_zazzcat_decode_into(
        source: *const u8,
        source_length: usize,
        destination: *mut u8,
        capacity: usize,
        decoded: *mut usize,
    ) -> c_int;
}

#[cfg(target_os = "macos")]
#[link(name = "compression")]
extern "C" {
    fn compression_decode_buffer(
        destination: *mut u8,
        capacity: usize,
        source: *const u8,
        source_length: usize,
        scratch: *mut c_void,
        algorithm: c_int,
    ) -> usize;
}

// Apple's libcompression algorithm number for LZFSE (COMPRESSION_LZFSE).
#[cfg(target_os = "macos")]
const COMPRESSION_LZFSE: c_int = 0x801;

// The decoder round trips use, named in reports.
pub const DECODER: &str = if cfg!(target_os = "macos") {
    "Apple's libcompression"
} else {
    "the vendored decoder (Apple's libcompression is macOS-only)"
};

// The decmpfs header magic and the compression types the library writes, from include/lzfse_zazzcat.h.
const MAGIC: &[u8; 4] = b"fpmc";
const LZFSE_INLINE: u32 = 11;
const LZFSE_FORK: u32 = 12;
const CHUNK: usize = 65536;

// Status codes from include/lzfse_zazzcat.h that the wrapper treats specially.
const OK: c_int = 0;
const UNSUPPORTED: c_int = -6;
const SYSTEM_ERROR: c_int = -7;

// A read-only memory map of a whole file, unmapped on drop.
struct Mapping {
    addr: *mut c_void,
    len: usize,
}

// Unmaps the file when the mapping goes out of scope.
impl Drop for Mapping {
    // Releases the mapping; a zero-length file was never mapped.
    fn drop(&mut self) {
        if self.len > 0 {
            // SAFETY: addr and len come from a successful mmap that nothing else unmaps.
            unsafe { libc::munmap(self.addr, self.len) };
        }
    }
}

// Returns the bytes of a library buffer.
fn bytes(buffer: &Buffer) -> &[u8] {
    if buffer.data.is_null() {
        return &[];
    }
    // SAFETY: the library keeps `length` initialized bytes at `data` until the buffer is freed.
    unsafe { std::slice::from_raw_parts(buffer.data, buffer.length) }
}

// Maps `file` read-only so the library can compress it without copying it into memory first.
fn map(file: &File) -> std::io::Result<Mapping> {
    let len = usize::try_from(file.metadata()?.len()).map_err(std::io::Error::other)?;
    if len == 0 {
        return Ok(Mapping { addr: std::ptr::null_mut(), len: 0 });
    }
    // SAFETY: a fresh private read-only mapping of an open file descriptor for its current length.
    let addr = unsafe {
        libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0)
    };
    if addr == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Mapping { addr, len })
}

// Describes a library status code in words.
fn status_text(status: c_int) -> String {
    // SAFETY: the library returns a static NUL-terminated string for every status.
    unsafe { CStr::from_ptr(lzfse_zazzcat_status_string(status)) }.to_string_lossy().into_owned()
}

// Turns a failed library call into an error naming the step and, for system calls, the OS reason.
fn failure(status: c_int, step: *const c_char) -> std::io::Error {
    let os = std::io::Error::last_os_error();
    let step = if step.is_null() {
        "compressing".to_string()
    } else {
        // SAFETY: the library sets failed_step to a static NUL-terminated string.
        unsafe { CStr::from_ptr(step) }.to_string_lossy().into_owned()
    };
    if status == SYSTEM_ERROR {
        return std::io::Error::new(os.kind(), format!("lzfse: {step}: {os}"));
    }
    std::io::Error::other(format!("lzfse: {step}: {}", status_text(status)))
}

// Decodes one LZFSE stream into `out` and returns the number of bytes written (0 when the decoder rejects it).
fn decode_stream(stream: &[u8], out: &mut [u8]) -> usize {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: both slices are valid for their lengths; libcompression allocates its own scratch for NULL.
        unsafe {
            compression_decode_buffer(
                out.as_mut_ptr(),
                out.len(),
                stream.as_ptr(),
                stream.len(),
                std::ptr::null_mut(),
                COMPRESSION_LZFSE,
            )
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut decoded = 0;
        // SAFETY: both slices are valid for their lengths and `decoded` is writable.
        let status = unsafe {
            lzfse_zazzcat_decode_into(
                stream.as_ptr(),
                stream.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut decoded,
            )
        };
        if status == OK {
            decoded
        } else {
            0
        }
    }
}

// Checks one chunk decodes to `expected`, as the kernel reads it: a stream starts with 'b', anything else is the
// chunk stored after a one-byte marker. `out` is scratch space reused between chunks.
fn check_chunk(chunk: &[u8], expected: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    match chunk.first() {
        None => return Err("empty chunk".into()),
        Some(b'b') => {}
        Some(_) if &chunk[1..] == expected => return Ok(()),
        Some(_) => return Err("stored chunk differs".into()),
    }
    // One byte of room past the expected size shows a stream that decodes too long.
    out.clear();
    out.resize(expected.len() + 1, 0);
    let n = decode_stream(chunk, out);
    if n != expected.len() {
        return Err(format!("decoded {n} bytes, expected {}", expected.len()));
    }
    match out[..n].iter().zip(expected).position(|(a, b)| a != b) {
        Some(at) => Err(format!("decoded bytes differ at offset {at}")),
        None => Ok(()),
    }
}

// Checks the decmpfs attributes `xattr` and `fork` decode to exactly `original`, and says how they differ if not.
fn check_decmpfs(xattr: &[u8], fork: &[u8], original: &[u8]) -> Result<(), String> {
    if xattr.len() < 16 || &xattr[..4] != MAGIC {
        return Err("bad decmpfs header".into());
    }
    let kind = u32::from_le_bytes(xattr[4..8].try_into().unwrap());
    let size = u64::from_le_bytes(xattr[8..16].try_into().unwrap());
    if size != original.len() as u64 {
        return Err(format!("header says {size} bytes, file has {}", original.len()));
    }
    let mut out = Vec::new();
    if kind == LZFSE_INLINE {
        return check_chunk(&xattr[16..], original, &mut out);
    }
    if kind != LZFSE_FORK {
        return Err(format!("unexpected compression type {kind}"));
    }
    let offset =
        |i: usize| fork.get(4 * i..4 * i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    for (i, expected) in original.chunks(CHUNK).enumerate() {
        let chunk = match (offset(i), offset(i + 1)) {
            (Some(start), Some(end)) if start <= end && end <= fork.len() => &fork[start..end],
            _ => return Err(format!("chunk {i}: offset table out of range")),
        };
        check_chunk(chunk, expected, &mut out).map_err(|e| format!("chunk {i}: {e}"))?;
    }
    Ok(())
}

// The result of compressing a file in memory and decoding it again.
pub struct RoundTrip {
    pub stored: u64,
    pub mismatch: Option<String>,
}

// Compresses the contents of `source` in memory exactly as compress_onto would and checks that DECODER turns the
// compressed form back into the same bytes; changes nothing on disk.
pub fn round_trip(source: &File) -> std::io::Result<RoundTrip> {
    let mapping = map(source)?;
    let original: &[u8] = if mapping.len == 0 {
        &[]
    } else {
        // SAFETY: the mapping is valid and readable for `len` bytes while `mapping` lives.
        unsafe { std::slice::from_raw_parts(mapping.addr as *const u8, mapping.len) }
    };
    let mut compressed = Decmpfs {
        xattr: Buffer { data: std::ptr::null_mut(), length: 0, capacity: 0 },
        fork: Buffer { data: std::ptr::null_mut(), length: 0, capacity: 0 },
    };
    // SAFETY: the mapping is valid for its length (or empty), and `compressed` is a writable result struct.
    let status = unsafe { lzfse_zazzcat_decmpfs_compress(mapping.addr, mapping.len, &mut compressed) };
    if status != OK {
        return Err(failure(status, std::ptr::null()));
    }
    let (xattr, fork) = (bytes(&compressed.xattr), bytes(&compressed.fork));
    let result = RoundTrip {
        stored: (xattr.len() + fork.len()) as u64,
        mismatch: check_decmpfs(xattr, fork, original).err(),
    };
    // SAFETY: frees the buffers the library allocated in decmpfs_compress, exactly once.
    unsafe { lzfse_zazzcat_decmpfs_free(&mut compressed) };
    Ok(result)
}

// Compresses the contents of `source` and makes `target`, an open writable copy of it, a transparently
// compressed file holding them. Outside macOS the attributes cannot be applied, so `target` stays a plain copy.
pub fn compress_onto(source: &File, target: &File) -> std::io::Result<()> {
    let mapping = map(source)?;
    let mut compressed = Decmpfs {
        xattr: Buffer { data: std::ptr::null_mut(), length: 0, capacity: 0 },
        fork: Buffer { data: std::ptr::null_mut(), length: 0, capacity: 0 },
    };
    // SAFETY: the mapping is valid for its length (or empty), and `compressed` is a writable result struct.
    let status = unsafe { lzfse_zazzcat_decmpfs_compress(mapping.addr, mapping.len, &mut compressed) };
    if status != OK {
        return Err(failure(status, std::ptr::null()));
    }
    let mut step: *const c_char = std::ptr::null();
    // SAFETY: target is an open descriptor, `compressed` holds the library's result and `step` is writable.
    let status = unsafe { lzfse_zazzcat_decmpfs_apply(target.as_raw_fd(), &compressed, &mut step) };
    let result = match status {
        OK => Ok(()),
        UNSUPPORTED if !cfg!(target_os = "macos") => Ok(()),
        _ => Err(failure(status, step)),
    };
    // SAFETY: frees the buffers the library allocated in decmpfs_compress, exactly once.
    unsafe { lzfse_zazzcat_decmpfs_free(&mut compressed) };
    result
}

// Unit tests for the library wrapper.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sandbox;

    // Check a file compresses through the library and, off macOS, the copy keeps its plain contents.
    #[test]
    fn test_compress_onto_plain_copy_off_macos() {
        let sb = Sandbox::new();
        let text = b"free-disk-space hands this file to the LZFSE library. ".repeat(4000);
        let src = sb.write("src.txt", &text);
        let dst = sb.write("dst.txt", &text);
        let target = std::fs::OpenOptions::new().read(true).write(true).open(&dst).unwrap();
        let result = compress_onto(&File::open(&src).unwrap(), &target);
        if cfg!(target_os = "macos") {
            return;
        }
        result.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), text);
    }

    // Check an empty file is handled without mapping it.
    #[test]
    fn test_compress_onto_empty_file() {
        let sb = Sandbox::new();
        let src = sb.write("empty", b"");
        let target = std::fs::OpenOptions::new().read(true).write(true).open(&src).unwrap();
        let result = compress_onto(&File::open(&src).unwrap(), &target);
        if !cfg!(target_os = "macos") {
            result.unwrap();
        }
    }

    // Compress `data` with the library and return its decmpfs attributes as (xattr, fork).
    fn attrs(data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut c = Decmpfs {
            xattr: Buffer { data: std::ptr::null_mut(), length: 0, capacity: 0 },
            fork: Buffer { data: std::ptr::null_mut(), length: 0, capacity: 0 },
        };
        assert_eq!(unsafe { lzfse_zazzcat_decmpfs_compress(data.as_ptr().cast(), data.len(), &mut c) }, OK);
        let pair = (bytes(&c.xattr).to_vec(), bytes(&c.fork).to_vec());
        unsafe { lzfse_zazzcat_decmpfs_free(&mut c) };
        pair
    }

    // Some bytes that do not compress, from a fixed linear congruential generator.
    fn noise(n: usize) -> Vec<u8> {
        let mut x: u32 = 12345;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (x >> 16) as u8
            })
            .collect()
    }

    // Check files of every shape round-trip byte-identical: empty, inline, several chunks and incompressible.
    #[test]
    fn test_round_trip_identical() {
        let sb = Sandbox::new();
        let mut mixed = b"many chunks of text. ".repeat(10_000);
        mixed.extend(noise(100_000));
        for (name, data) in [
            ("empty", Vec::new()),
            ("small", b"inline in the attribute ".repeat(40)),
            ("mixed", mixed),
            ("noise", noise(3 * CHUNK + 7)),
        ] {
            let path = sb.write(name, &data);
            let r = round_trip(&File::open(&path).unwrap()).unwrap();
            assert_eq!(r.mismatch, None, "{name}");
            assert!(r.stored >= 16, "{name}");
        }
        let (xattr, fork) = attrs(&b"inline in the attribute ".repeat(40));
        assert_eq!(u32::from_le_bytes(xattr[4..8].try_into().unwrap()), LZFSE_INLINE);
        assert!(fork.is_empty());
    }

    // Check every way a compressed form can fail to decode back to the original is caught and described.
    #[test]
    fn test_check_decmpfs_mismatches() {
        let text = b"round trip me through the decoder. ".repeat(5000);
        let (xattr, fork) = attrs(&text);
        assert_eq!(check_decmpfs(&xattr, &fork, &text), Ok(()));
        assert_eq!(check_decmpfs(&xattr[..8], &fork, &text).unwrap_err(), "bad decmpfs header");
        assert!(check_decmpfs(&xattr, &fork, &text[1..]).unwrap_err().starts_with("header says"));
        let mut other = xattr.clone();
        other[4] = 8;
        assert_eq!(check_decmpfs(&other, &fork, &text).unwrap_err(), "unexpected compression type 8");
        assert_eq!(
            check_decmpfs(&xattr, &fork[..8], &text).unwrap_err(),
            "chunk 0: offset table out of range"
        );
        let mut changed = text.clone();
        changed[CHUNK + 5] ^= 1;
        assert_eq!(
            check_decmpfs(&xattr, &fork, &changed).unwrap_err(),
            "chunk 1: decoded bytes differ at offset 5"
        );
        let mut cut = fork.clone();
        let end = u32::from_le_bytes(cut[4..8].try_into().unwrap()) as usize;
        cut[end - 2] ^= 0xff;
        assert!(check_decmpfs(&xattr, &cut, &text).unwrap_err().starts_with("chunk 0: decoded"));
        let mut out = Vec::new();
        assert_eq!(check_chunk(b"", b"x", &mut out).unwrap_err(), "empty chunk");
        assert_eq!(check_chunk(b"\x06abc", b"abc", &mut out), Ok(()));
        assert_eq!(check_chunk(b"\x06abd", b"abc", &mut out).unwrap_err(), "stored chunk differs");
        assert_eq!(check_chunk(b"bvx2junk", b"abc", &mut out).unwrap_err(), "decoded 0 bytes, expected 3");
    }

    // Check a failed step is reported with its name and the status in words.
    #[test]
    fn test_failure_names_the_step() {
        let step = c"setting com.apple.decmpfs";
        let err = failure(-8, step.as_ptr());
        assert!(err.to_string().contains("setting com.apple.decmpfs"));
        assert!(err.to_string().contains("did not take the compressed form"));
        assert!(failure(-2, std::ptr::null()).to_string().starts_with("lzfse: compressing:"));
    }
}
