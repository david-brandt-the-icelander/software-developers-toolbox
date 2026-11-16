// Reads and restores the file metadata a compressed copy must keep: extended attribute values, the ACL, flags and the birth time.
use std::collections::BTreeMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs::{File, Metadata};
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::util::cstring;

// Attributes that hold the compressed form itself rather than the file's metadata.
const COMPRESSION_XATTRS: [&str; 2] = ["com.apple.decmpfs", "com.apple.ResourceFork"];

// List a file's extended attribute names as NUL-separated bytes, without following a symlink.
#[cfg(target_os = "macos")]
fn list_raw(path: &CStr, buf: *mut c_char, size: usize) -> isize {
    // SAFETY: path is NUL-terminated and buf is null (size query) or valid for size bytes.
    unsafe { libc::listxattr(path.as_ptr(), buf, size, libc::XATTR_NOFOLLOW) }
}

// List a file's extended attribute names as NUL-separated bytes, without following a symlink.
#[cfg(not(target_os = "macos"))]
fn list_raw(path: &CStr, buf: *mut c_char, size: usize) -> isize {
    // SAFETY: path is NUL-terminated and buf is null (size query) or valid for size bytes.
    unsafe { libc::llistxattr(path.as_ptr(), buf, size) }
}

// Let getxattr read the compression attributes, which macOS hides on a compressed file (sys/xattr.h).
#[cfg(target_os = "macos")]
const XATTR_SHOWCOMPRESSION: libc::c_int = 0x0020;

// Read one extended attribute's value into buf, without following a symlink, including hidden compression ones.
#[cfg(target_os = "macos")]
fn get_raw(path: &CStr, name: &CStr, buf: *mut c_void, size: usize) -> isize {
    let options = libc::XATTR_NOFOLLOW | XATTR_SHOWCOMPRESSION;
    // SAFETY: both strings are NUL-terminated and buf is null (size query) or valid for size bytes.
    unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), buf, size, 0, options) }
}

// Read one extended attribute's value into buf, without following a symlink.
#[cfg(not(target_os = "macos"))]
fn get_raw(path: &CStr, name: &CStr, buf: *mut c_void, size: usize) -> isize {
    // SAFETY: both strings are NUL-terminated and buf is null (size query) or valid for size bytes.
    unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), buf, size) }
}

// Run a size-then-read call pair until the buffer was large enough for a result that may change in between.
fn read_sized(call: impl Fn(*mut u8, usize) -> isize) -> std::io::Result<Vec<u8>> {
    loop {
        let size = call(std::ptr::null_mut(), 0);
        if size < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut buf = vec![0u8; size as usize];
        let got = call(buf.as_mut_ptr(), buf.len());
        if got >= 0 {
            buf.truncate(got as usize);
            return Ok(buf);
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ERANGE) {
            return Err(std::io::Error::last_os_error());
        }
    }
}

// Return all of a file's extended attributes with their values, minus the compression attributes. A filesystem
// without extended attributes gives an empty map.
pub fn xattrs(path: &Path) -> std::io::Result<BTreeMap<String, Vec<u8>>> {
    let cpath = cstring(path)?;
    let names = match read_sized(|buf, size| list_raw(&cpath, buf.cast(), size)) {
        Ok(names) => names,
        Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    let mut out = BTreeMap::new();
    for raw in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let name = String::from_utf8_lossy(raw).into_owned();
        if COMPRESSION_XATTRS.contains(&name.as_str()) {
            continue;
        }
        let cname = CString::new(raw).map_err(std::io::Error::other)?;
        let value = read_sized(|buf, size| get_raw(&cpath, &cname, buf.cast(), size))?;
        out.insert(name, value);
    }
    Ok(out)
}

// The error a missing extended attribute gives (ENOATTR on macOS, ENODATA on Linux).
#[cfg(target_os = "macos")]
const NO_ATTR: i32 = libc::ENOATTR;
// The error a missing extended attribute gives on Linux.
#[cfg(not(target_os = "macos"))]
const NO_ATTR: i32 = libc::ENODATA;

// Return one extended attribute's value, or None when the file does not have it.
pub fn xattr(path: &Path, name: &str) -> std::io::Result<Option<Vec<u8>>> {
    let (cpath, cname) = (cstring(path)?, CString::new(name).map_err(std::io::Error::other)?);
    match read_sized(|buf, size| get_raw(&cpath, &cname, buf.cast(), size)) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.raw_os_error() == Some(NO_ATTR) => Ok(None),
        Err(e) => Err(e),
    }
}

// Set an extended attribute without following a symlink.
pub fn set_xattr(path: &Path, name: &str, value: &[u8]) -> std::io::Result<()> {
    let (p, n) = (cstring(path)?, CString::new(name).map_err(std::io::Error::other)?);
    // SAFETY: valid NUL-terminated strings and a value slice valid for its length.
    #[cfg(target_os = "macos")]
    let rc = unsafe {
        libc::setxattr(p.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0, libc::XATTR_NOFOLLOW)
    };
    // SAFETY: valid NUL-terminated strings and a value slice valid for its length.
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe { libc::lsetxattr(p.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// Remove an extended attribute without following a symlink, falling back to /usr/bin/xattr where macOS refuses.
pub fn remove_xattr(path: &Path, name: &str) -> std::io::Result<()> {
    let (p, n) = (cstring(path)?, CString::new(name).map_err(std::io::Error::other)?);
    // SAFETY: valid NUL-terminated strings.
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::removexattr(p.as_ptr(), n.as_ptr(), libc::XATTR_NOFOLLOW) };
    // SAFETY: valid NUL-terminated strings.
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe { libc::lremovexattr(p.as_ptr(), n.as_ptr()) };
    if rc == 0 {
        return Ok(());
    }
    let direct = std::io::Error::last_os_error();
    let target = path.to_string_lossy();
    match crate::util::run(&["/usr/bin/xattr", "-s", "-d", name, &target], 60.0, &[]) {
        Some(out) if out.code == 0 => Ok(()),
        Some(out) => Err(std::io::Error::other(format!("{direct}; xattr -d: {}", out.stderr.trim()))),
        None => Err(std::io::Error::other(format!("{direct}; xattr -d did not finish"))),
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn acl_get_link_np(path: *const c_char, acl_type: libc::c_int) -> *mut c_void;
    fn acl_to_text(acl: *mut c_void, len: *mut isize) -> *mut c_char;
    fn acl_free(obj: *mut c_void) -> libc::c_int;
    fn acl_set_fd_np(fd: libc::c_int, acl: *mut c_void, acl_type: libc::c_int) -> libc::c_int;
}

// The extended (NFSv4-style) ACL type, the only kind macOS files carry (sys/acl.h).
#[cfg(target_os = "macos")]
const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;

// Name the extended attributes of `from` that `to` lacks or holds with a different value.
pub fn differing_xattrs(from: &Path, to: &Path) -> std::io::Result<Vec<String>> {
    let have = xattrs(to)?;
    Ok(xattrs(from)?
        .into_iter()
        .filter(|(name, value)| have.get(name) != Some(value))
        .map(|(name, _)| name)
        .collect())
}

// Set an extended attribute with Apple's /usr/bin/xattr, which macOS lets set attributes it refuses other programs.
fn set_xattr_with_tool(path: &Path, name: &str, value: &[u8]) -> std::io::Result<()> {
    let hex: String = value.iter().map(|b| format!("{b:02x}")).collect();
    let target = path.to_string_lossy();
    match crate::util::run(&["/usr/bin/xattr", "-s", "-wx", name, &hex, &target], 60.0, &[]) {
        Some(out) if out.code == 0 => Ok(()),
        Some(out) => Err(std::io::Error::other(format!("xattr -wx: {}", out.stderr.trim()))),
        None => Err(std::io::Error::other("xattr -wx did not finish")),
    }
}

// Give `to` every extended attribute value of `from` that it lacks or holds differently, such as the provenance
// macOS stamps on a new file, falling back to /usr/bin/xattr; fail naming each value that still differs and why.
pub fn copy_xattrs(from: &Path, to: &Path) -> std::io::Result<()> {
    let values = xattrs(from)?;
    let mut refused = BTreeMap::new();
    for name in differing_xattrs(from, to)? {
        if let Err(direct) = set_xattr(to, &name, &values[&name]) {
            if let Err(tool) = set_xattr_with_tool(to, &name, &values[&name]) {
                refused.insert(name, format!("{direct}; {tool}"));
            }
        }
    }
    let still = differing_xattrs(from, to)?;
    if still.is_empty() {
        return Ok(());
    }
    let why = |name: &String| refused.get(name).map_or("set, then changed again".to_string(), |e| e.clone());
    let notes: Vec<String> = still.iter().map(|name| format!("{name} ({})", why(name))).collect();
    Err(std::io::Error::other(format!("cannot keep extended attributes: {}", notes.join(", "))))
}

// Return a file's ACL as text, or None when it has none, without following a symlink.
#[cfg(target_os = "macos")]
pub fn acl_text(path: &Path) -> std::io::Result<Option<String>> {
    let cpath = cstring(path)?;
    // SAFETY: cpath is NUL-terminated; the returned ACL is freed below.
    let acl = unsafe { acl_get_link_np(cpath.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let e = std::io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) { Ok(None) } else { Err(e) };
    }
    // SAFETY: acl is a valid ACL; the text is freed with acl_free as acl_to_text requires.
    let text = unsafe { acl_to_text(acl, std::ptr::null_mut()) };
    let result = if text.is_null() {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: acl_to_text returned a NUL-terminated string.
        Ok(Some(unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()))
    };
    // SAFETY: both were allocated by the ACL library and are freed once.
    unsafe {
        if !text.is_null() {
            acl_free(text.cast());
        }
        acl_free(acl);
    }
    result
}

// Give an open file the ACL of another file, if that one has an ACL.
#[cfg(target_os = "macos")]
pub fn copy_acl(from: &Path, to: &File) -> std::io::Result<()> {
    let cpath = cstring(from)?;
    // SAFETY: cpath is NUL-terminated; the returned ACL is freed below.
    let acl = unsafe { acl_get_link_np(cpath.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let e = std::io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOENT) { Ok(()) } else { Err(e) };
    }
    // SAFETY: a valid descriptor and ACL; the ACL is freed once afterwards.
    let rc = unsafe { acl_set_fd_np(to.as_raw_fd(), acl, ACL_TYPE_EXTENDED) };
    let result = if rc == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) };
    // SAFETY: allocated by acl_get_link_np and freed once.
    unsafe { acl_free(acl) };
    result
}

// Do nothing, since ACLs as copied here exist only on macOS.
#[cfg(not(target_os = "macos"))]
pub fn copy_acl(_from: &Path, _to: &File) -> std::io::Result<()> {
    Ok(())
}

// Give an open file these BSD flags.
#[cfg(target_os = "macos")]
pub fn set_flags(to: &File, flags: u32) -> std::io::Result<()> {
    // SAFETY: a valid descriptor; fchflags only changes the file's flags.
    if unsafe { libc::fchflags(to.as_raw_fd(), flags) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// Accept only no flags, since BSD file flags exist only on macOS.
#[cfg(not(target_os = "macos"))]
pub fn set_flags(to: &File, flags: u32) -> std::io::Result<()> {
    let _ = to.as_raw_fd();
    if flags != 0 {
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    Ok(())
}

// Report that a file has no ACL, since ACLs as compared here exist only on macOS.
#[cfg(not(target_os = "macos"))]
pub fn acl_text(_path: &Path) -> std::io::Result<Option<String>> {
    Ok(None)
}

// Return a file's birth (creation) time in nanoseconds, which only macOS reports through Metadata.
#[cfg(target_os = "macos")]
pub fn birth_ns(md: &Metadata) -> Option<i64> {
    use std::os::macos::fs::MetadataExt;
    Some(md.st_birthtime() * 1_000_000_000 + md.st_birthtime_nsec())
}

// Return no birth time, since only macOS reports and sets it here.
#[cfg(not(target_os = "macos"))]
pub fn birth_ns(_md: &Metadata) -> Option<i64> {
    None
}

// Set a file's birth (creation) time in nanoseconds without following a symlink.
#[cfg(target_os = "macos")]
pub fn set_birth(path: &Path, ns: i64) -> std::io::Result<()> {
    let cpath = cstring(path)?;
    let mut list = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_CRTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut time = libc::timespec {
        tv_sec: ns.div_euclid(1_000_000_000) as libc::time_t,
        tv_nsec: ns.rem_euclid(1_000_000_000) as _,
    };
    // SAFETY: a valid path, an attrlist asking for the creation time only, and a timespec buffer for it.
    let rc = unsafe {
        libc::setattrlist(
            cpath.as_ptr(),
            (&mut list as *mut libc::attrlist).cast(),
            (&mut time as *mut libc::timespec).cast(),
            std::mem::size_of::<libc::timespec>(),
            libc::FSOPT_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// Accept and ignore a birth time, which cannot be set off macOS.
#[cfg(not(target_os = "macos"))]
pub fn set_birth(_path: &Path, _ns: i64) -> std::io::Result<()> {
    Ok(())
}

// Say why a file with a real resource fork must be skipped: compression stores its data in that same attribute.
pub fn resource_fork_note(path: &Path) -> Option<String> {
    match xattr(path, "com.apple.ResourceFork") {
        Ok(None) => None,
        Ok(Some(_)) => Some("has a resource fork".into()),
        Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => None,
        Err(e) => Some(format!("cannot read resource fork: {e}")),
    }
}

// Tell whether two files have the same BSD flags apart from the compressed flag.
pub fn same_flags(a: u32, b: u32) -> bool {
    a & !crate::util::UF_COMPRESSED == b & !crate::util::UF_COMPRESSED
}

// Tell whether two files have the same access, modification and (where reported) birth times.
pub fn same_times(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let times =
        |m: &Metadata| (m.atime() * 1_000_000_000 + m.atime_nsec(), crate::util::mtime_ns(m), birth_ns(m));
    times(a) == times(b)
}

// Unit tests for the metadata helpers.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{set_xattr, Sandbox};

    // Check attribute values are read, compression attributes are left out, and a missing file is an error.
    #[test]
    fn test_xattrs_values() {
        let sb = Sandbox::new();
        let file = sb.write("f", b"x");
        let name = if cfg!(target_os = "macos") { "com.example.tag" } else { "user.tag" };
        if !set_xattr(&file, name, b"blue") {
            return;
        }
        let all = xattrs(&file).unwrap();
        assert_eq!(all.get(name).map(Vec::as_slice), Some(&b"blue"[..]));
        assert!(xattrs(&sb.path("missing")).is_err());
        assert_eq!(xattr(&file, name).unwrap().as_deref(), Some(&b"blue"[..]));
        assert_eq!(xattr(&file, "user.absent").unwrap(), None);
        assert!(xattr(&sb.path("missing"), name).is_err());
    }

    // Check a plain file reports no ACL and, off macOS, no birth time.
    #[test]
    fn test_acl_and_birth_defaults() {
        let sb = Sandbox::new();
        let file = sb.write("f", b"x");
        assert_eq!(acl_text(&file).unwrap(), None);
        let md = std::fs::symlink_metadata(&file).unwrap();
        if cfg!(target_os = "macos") {
            set_birth(&file, 1_000_000_000_000_000_000).unwrap();
            assert_eq!(birth_ns(&std::fs::symlink_metadata(&file).unwrap()), Some(1_000_000_000_000_000_000));
        } else {
            assert_eq!(birth_ns(&md), None);
            set_birth(&file, 0).unwrap();
            assert!(set_flags(&std::fs::File::open(&file).unwrap(), 1).is_err());
        }
        let copy = sb.write("g", b"y");
        let target = std::fs::File::open(&copy).unwrap();
        copy_acl(&file, &target).unwrap();
        set_flags(&target, 0).unwrap();
        assert_eq!(acl_text(&copy).unwrap(), None);
    }

    // Check flags compare without the compressed flag and times compare exactly.
    #[test]
    fn test_same_flags_and_times() {
        assert!(same_flags(0x8000, 0x8000 | crate::util::UF_COMPRESSED));
        assert!(!same_flags(0x8000, crate::util::UF_COMPRESSED));
        let sb = Sandbox::new();
        let (a, b) = (sb.write("a", b"a"), sb.write("b", b"b"));
        crate::util::set_times(&a, 1_000_000_000_000_000_000, 1_100_000_000_000_000_000).unwrap();
        crate::util::set_times(&b, 1_000_000_000_000_000_000, 1_100_000_000_000_000_000).unwrap();
        set_birth(&a, 900_000_000_000_000_000).unwrap();
        set_birth(&b, 900_000_000_000_000_000).unwrap();
        let md = |p: &Path| std::fs::symlink_metadata(p).unwrap();
        assert!(same_times(&md(&a), &md(&b)));
        assert!(same_times(&md(&a), &md(&a)));
        crate::util::set_times(&b, 1_000_000_000_000_000_000, 1_100_000_000_000_000_001).unwrap();
        assert!(!same_times(&md(&a), &md(&b)));
    }

    // Check attribute values are copied where missing or different, and a missing file is an error.
    #[test]
    fn test_copy_xattrs() {
        let sb = Sandbox::new();
        let (from, to) = (sb.write("from", b"a"), sb.write("to", b"b"));
        let name = if cfg!(target_os = "macos") { "com.example.tag" } else { "user.tag" };
        let other = if cfg!(target_os = "macos") { "com.example.other" } else { "user.other" };
        if !set_xattr(&from, name, b"blue") || !set_xattr(&to, name, b"red") {
            return;
        }
        assert!(set_xattr(&from, other, b"x"));
        assert_eq!(differing_xattrs(&from, &to).unwrap(), [other, name].map(String::from).to_vec());
        copy_xattrs(&from, &to).unwrap();
        assert!(differing_xattrs(&from, &to).unwrap().is_empty());
        assert_eq!(xattrs(&to).unwrap(), xattrs(&from).unwrap());
        assert!(copy_xattrs(&sb.path("missing"), &to).is_err());
    }

    // Check an attribute is removed directly, and one the direct call cannot remove goes to /usr/bin/xattr -d.
    #[test]
    fn test_remove_xattr() {
        let sb = Sandbox::new();
        let file = sb.write("f", b"x");
        let name = if cfg!(target_os = "macos") { "com.example.tag" } else { "user.tag" };
        if !set_xattr(&file, name, b"blue") {
            return;
        }
        remove_xattr(&file, name).unwrap();
        assert_eq!(xattr(&file, name).unwrap(), None);
        sb.tool("xattr", &format!("[ \"$1 $2 $3\" = '-s -d {name}' ] || exit 3"));
        remove_xattr(&file, name).unwrap();
        sb.tool("xattr", "echo refused >&2; exit 1");
        let err = remove_xattr(&file, name).unwrap_err().to_string();
        assert!(err.ends_with("; xattr -d: refused"), "{err}");
    }

    // Check a value the direct call cannot set goes through /usr/bin/xattr with its hex value, and a value that still
    // differs is named with the reason.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_copy_xattrs_fallback() {
        let sb = Sandbox::new();
        let (from, to) = (sb.write("from", b"a"), sb.write("to", b"b"));
        if !set_xattr(&from, "user.tag", b"blue") {
            return;
        }
        // Linux refuses user attributes on a symlink itself, so the direct call fails and the tool is used.
        let link = sb.path("link");
        std::os::unix::fs::symlink(&to, &link).unwrap();
        sb.tool("xattr", "[ \"$1 $2 $3 $4\" = '-s -wx user.tag 626c7565' ] || exit 3");
        let err = copy_xattrs(&from, &link).unwrap_err().to_string();
        assert_eq!(err, "cannot keep extended attributes: user.tag (set, then changed again)");
        sb.tool("xattr", "echo refused >&2; exit 1");
        let err = copy_xattrs(&from, &link).unwrap_err().to_string();
        assert!(
            err.starts_with("cannot keep extended attributes: user.tag (")
                && err.ends_with("xattr -wx: refused)"),
            "{err}"
        );
    }
}
