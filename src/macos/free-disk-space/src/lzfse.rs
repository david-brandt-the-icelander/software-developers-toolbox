// Safe wrapper over the vendored lempel-ziv-finite-state-entropy-compression C library: compress a file into APFS transparent compression.
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
}

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
