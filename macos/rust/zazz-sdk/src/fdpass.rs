// Passing file descriptors over Unix sockets (SCM_RIGHTS) with no libc crate: just the C structs per platform.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::raw::{c_int, c_void};

const SCM_RIGHTS: c_int = 1;
const MSG_PEEK: c_int = 2;
#[cfg(target_os = "linux")]
const MSG_DONTWAIT: c_int = 0x40;
#[cfg(not(target_os = "linux"))]
const MSG_DONTWAIT: c_int = 0x80;
#[cfg(target_os = "linux")]
const SOL_SOCKET: c_int = 1;
#[cfg(not(target_os = "linux"))]
const SOL_SOCKET: c_int = 0xffff;
const MAX_FDS: usize = 8;

// One buffer for scatter/gather I/O.
#[repr(C)]
struct IoVec {
    base: *mut c_void,
    len: usize,
}

// The message header for sendmsg/recvmsg on Linux.
#[cfg(target_os = "linux")]
#[repr(C)]
struct MsgHdr {
    name: *mut c_void,
    namelen: u32,
    iov: *mut IoVec,
    iovlen: usize,
    control: *mut c_void,
    controllen: usize,
    flags: c_int,
}

// The message header for sendmsg/recvmsg on BSD and macOS.
#[cfg(not(target_os = "linux"))]
#[repr(C)]
struct MsgHdr {
    name: *mut c_void,
    namelen: u32,
    iov: *mut IoVec,
    iovlen: c_int,
    control: *mut c_void,
    controllen: u32,
    flags: c_int,
}

#[cfg(target_os = "linux")]
type CmsgLen = usize;
#[cfg(not(target_os = "linux"))]
type CmsgLen = u32;

// A control message header; descriptor data follows, aligned.
#[repr(C)]
struct CmsgHdr {
    len: CmsgLen,
    level: c_int,
    kind: c_int,
}

extern "C" {
    fn sendmsg(fd: c_int, msg: *const MsgHdr, flags: c_int) -> isize;
    fn recvmsg(fd: c_int, msg: *mut MsgHdr, flags: c_int) -> isize;
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
}

// Rounds up to the platform's control-message alignment.
fn align(n: usize) -> usize {
    let a = std::mem::size_of::<CmsgLen>();
    n.div_ceil(a) * a
}

// The byte offset of descriptor data after the control header.
fn data_offset() -> usize {
    align(std::mem::size_of::<CmsgHdr>())
}

// Sends bytes with descriptors attached in one sendmsg; retries interrupted calls.
pub fn send_with_fds(sock: RawFd, bytes: &[u8], fds: &[RawFd]) -> io::Result<()> {
    let data_len = std::mem::size_of_val(fds);
    let mut ctl = vec![0u64; (data_offset() + align(data_len)).div_ceil(8)];
    let mut iov = IoVec { base: bytes.as_ptr() as *mut c_void, len: bytes.len() };
    let mut msg = MsgHdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: std::ptr::null_mut(),
        controllen: 0,
        flags: 0,
    };
    if !fds.is_empty() {
        let hdr = CmsgHdr { len: (data_offset() + data_len) as CmsgLen, level: SOL_SOCKET, kind: SCM_RIGHTS };
        // SAFETY: ctl holds at least data_offset() + data_len bytes, 8-byte aligned, and the header fits at its start.
        unsafe {
            std::ptr::write(ctl.as_mut_ptr().cast::<CmsgHdr>(), hdr);
            let data = ctl.as_mut_ptr().cast::<u8>().add(data_offset()).cast::<RawFd>();
            std::ptr::copy_nonoverlapping(fds.as_ptr(), data, fds.len());
        }
        msg.control = ctl.as_mut_ptr().cast();
        msg.controllen = (data_offset() + align(data_len)) as _;
    }
    let mut sent = 0;
    while sent < bytes.len() {
        // SAFETY: msg points at live buffers for the duration of the call.
        let n = unsafe { sendmsg(sock, &msg, 0) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        sent += n as usize;
        iov.base = bytes[sent..].as_ptr() as *mut c_void;
        iov.len = bytes.len() - sent;
        msg.iov = &mut iov;
        msg.control = std::ptr::null_mut();
        msg.controllen = 0;
    }
    Ok(())
}

// Collects descriptors from the control buffer of a received message.
fn take_fds(ctl: &[u64], controllen: usize) -> Vec<OwnedFd> {
    let mut out = Vec::new();
    let base = ctl.as_ptr().cast::<u8>();
    let mut off = 0;
    while off + data_offset() <= controllen {
        // SAFETY: off + header size lies inside the control buffer the kernel filled.
        let hdr = unsafe { std::ptr::read_unaligned(base.add(off).cast::<CmsgHdr>()) };
        let len = hdr.len as usize;
        if len < data_offset() || off + len > controllen {
            break;
        }
        if hdr.level == SOL_SOCKET && hdr.kind == SCM_RIGHTS {
            let n = (len - data_offset()) / std::mem::size_of::<RawFd>();
            for i in 0..n {
                // SAFETY: the kernel placed n descriptors after the header; each is a new descriptor we now own.
                let fd =
                    unsafe { std::ptr::read_unaligned(base.add(off + data_offset()).cast::<RawFd>().add(i)) };
                // SAFETY: fd was just received and is owned by nobody else.
                out.push(unsafe { OwnedFd::from_raw_fd(fd) });
            }
        }
        off += align(len);
    }
    out
}

// Receives up to buf.len() bytes and any descriptors sent with them.
pub fn recv_with_fds(sock: RawFd, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    let mut ctl = vec![0u64; (data_offset() + align(MAX_FDS * 4)).div_ceil(8) + 1];
    let mut iov = IoVec { base: buf.as_mut_ptr().cast(), len: buf.len() };
    let mut msg = MsgHdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: ctl.as_mut_ptr().cast(),
        controllen: (ctl.len() * 8) as _,
        flags: 0,
    };
    loop {
        // SAFETY: msg points at live buffers sized as declared.
        let n = unsafe { recvmsg(sock, &mut msg, 0) };
        if n >= 0 {
            return Ok((n as usize, take_fds(&ctl, msg.controllen as usize)));
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

// True if bytes are waiting on the socket (without consuming them or blocking).
pub fn pending(sock: &impl AsRawFd) -> bool {
    let mut b = [0u8; 1];
    // SAFETY: b is a live one-byte buffer.
    let n = unsafe { recv(sock.as_raw_fd(), b.as_mut_ptr().cast(), 1, MSG_PEEK | MSG_DONTWAIT) };
    n > 0
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    // Checks descriptors travel with the bytes and can be used on the other side.
    #[test]
    fn test_send_recv_fds() {
        let (a, b) = UnixStream::pair().expect("pair");
        let (mut r, w) = UnixStream::pair().expect("pipe-like pair");
        send_with_fds(a.as_raw_fd(), b"hello", &[w.as_raw_fd()]).expect("send");
        drop(w);
        let mut buf = [0u8; 16];
        let (n, fds) = recv_with_fds(b.as_raw_fd(), &mut buf).expect("recv");
        assert_eq!((&buf[..n], fds.len()), (&b"hello"[..], 1));
        let mut passed = std::fs::File::from(fds.into_iter().next().expect("fd"));
        passed.write_all(b"via fd").expect("write");
        drop(passed);
        let mut s = String::new();
        r.read_to_string(&mut s).expect("read");
        assert_eq!(s, "via fd");
    }

    // Checks plain sends, pending detection and errors on bad descriptors.
    #[test]
    fn test_plain_pending_and_errors() {
        let (a, b) = UnixStream::pair().expect("pair");
        assert!(!pending(&b));
        send_with_fds(a.as_raw_fd(), b"x", &[]).expect("send");
        assert!(pending(&b));
        let mut buf = [0u8; 4];
        let (n, fds) = recv_with_fds(b.as_raw_fd(), &mut buf).expect("recv");
        assert_eq!((n, fds.len()), (1, 0));
        assert!(send_with_fds(-1, b"x", &[]).is_err());
        assert!(recv_with_fds(-1, &mut buf).is_err());
    }

    // Checks a malformed control buffer yields no descriptors.
    #[test]
    fn test_take_fds_ignores_garbage() {
        let mut ctl = vec![0u64; 4];
        assert!(take_fds(&ctl, 32).is_empty());
        let hdr = CmsgHdr { len: (data_offset() + 4) as CmsgLen, level: 7, kind: SCM_RIGHTS };
        // SAFETY: ctl is large enough for one header.
        unsafe { std::ptr::write(ctl.as_mut_ptr().cast::<CmsgHdr>(), hdr) };
        assert!(take_fds(&ctl, 32).is_empty());
        let big = CmsgHdr { len: 1000, level: SOL_SOCKET, kind: SCM_RIGHTS };
        // SAFETY: as above.
        unsafe { std::ptr::write(ctl.as_mut_ptr().cast::<CmsgHdr>(), big) };
        assert!(take_fds(&ctl, 32).is_empty());
    }
}
