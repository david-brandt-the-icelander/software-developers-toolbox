// Phase 0 spike daemon: answers afo requests over a unix socket so round-trip cost can be measured.
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;

const MAX_REQUEST: usize = 65536;
const MAX_FDS: usize = 4;
const DEFAULT_PAYLOAD: usize = 16;
const MAX_PAYLOAD: usize = 1 << 20;

// A decoded request: the invoker's arguments and any file descriptors it passed.
struct Request {
    args: Vec<Vec<u8>>,
    fds: Vec<OwnedFd>,
}

// Splits the NUL-separated argument block into its arguments.
fn split_args(block: &[u8]) -> Vec<Vec<u8>> {
    let mut parts: Vec<Vec<u8>> = block.split(|b| *b == 0).map(<[u8]>::to_vec).collect();
    if parts.last().is_some_and(Vec::is_empty) {
        parts.pop();
    }
    parts
}

// Reads the requested payload size from the first argument, clamped to a sane range.
fn payload_len(args: &[Vec<u8>]) -> usize {
    let parsed = args.first().and_then(|a| std::str::from_utf8(a).ok()).and_then(|s| s.parse::<usize>().ok());
    parsed.unwrap_or(DEFAULT_PAYLOAD).clamp(1, MAX_PAYLOAD)
}

// Builds a payload of n bytes made of 'x' and a final newline.
fn payload(n: usize) -> Vec<u8> {
    let mut body = vec![b'x'; n - 1];
    body.push(b'\n');
    body
}

// Collects the file descriptors carried by SCM_RIGHTS control messages.
fn take_fds(msg: &libc::msghdr) -> Vec<OwnedFd> {
    let mut out = Vec::new();
    // SAFETY: msg was filled by a successful recvmsg, so its control buffer is valid and the CMSG macros stay inside it.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(c) as *const RawFd;
                let bytes = (*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                for i in 0..bytes / mem::size_of::<RawFd>() {
                    out.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                }
            }
            c = libc::CMSG_NXTHDR(msg, c);
        }
    }
    out
}

// Receives bytes and any passed descriptors with one recvmsg call.
fn recv_with_fds(sock: RawFd, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    let mut ctl = [0u64; 16];
    // SAFETY: msghdr is plain data for which all-zero is a valid value.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = ctl.as_mut_ptr().cast();
    msg.msg_controllen = mem::size_of_val(&ctl) as _;
    // SAFETY: msg points at buffers that stay alive and are sized as declared.
    let n = unsafe { libc::recvmsg(sock, &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let fds = take_fds(&msg);
    if msg.msg_flags & libc::MSG_CTRUNC != 0 || fds.len() > MAX_FDS {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too many descriptors"));
    }
    Ok((n as usize, fds))
}

// Reads one complete length-prefixed request from the stream.
fn read_request(stream: &UnixStream) -> io::Result<Request> {
    let mut buf = vec![0u8; MAX_REQUEST + 4];
    let (mut have, fds) = recv_with_fds(stream.as_raw_fd(), &mut buf)?;
    if have < 4 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short request"));
    }
    let want = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if want > MAX_REQUEST {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "request too large"));
    }
    let mut reader = stream;
    while have < 4 + want {
        let n = reader.read(&mut buf[have..4 + want])?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        have += n;
    }
    Ok(Request { args: split_args(&buf[4..4 + want]), fds })
}

// Returns the uid of the peer on a connected unix socket.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    // SAFETY: ucred is plain data for which all-zero is a valid value.
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are live and sized as declared.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

// Returns the uid of the peer on a connected unix socket.
#[cfg(not(target_os = "linux"))]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: uid and gid are live out-parameters.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

// Writes the answer to the passed stdout when there is one, else back over the socket, then the status byte.
fn answer(mut stream: UnixStream, mut req: Request) -> io::Result<()> {
    let body = payload(payload_len(&req.args));
    if req.fds.len() >= 2 {
        let mut out = File::from(req.fds.swap_remove(1));
        out.write_all(&body)?;
        drop(out);
        return stream.write_all(&[0]);
    }
    let mut all = body;
    all.push(0);
    stream.write_all(&all)
}

// Serves one connection after checking the peer runs as the same user.
fn handle(stream: UnixStream, uid: u32) -> io::Result<()> {
    if peer_uid(&stream)? != uid {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "peer uid mismatch"));
    }
    let req = read_request(&stream)?;
    answer(stream, req)
}

// Returns the socket path from AFO_SOCK or the default under the home directory.
fn socket_path() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("AFO_SOCK") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".agent-file-operations").join("sock"))
}

// Creates the private socket directory and clears a stale socket, refusing if a daemon already answers.
fn prepare(path: &std::path::Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::other("a daemon is already running"));
        }
        std::fs::remove_file(path)?;
    }
    Ok(())
}

// Binds the socket and answers connections one at a time.
fn serve(path: &std::path::Path) -> io::Result<()> {
    prepare(path)?;
    // SAFETY: umask only changes this process's file-creation mask.
    unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(path)?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    for stream in listener.incoming() {
        if let Err(e) = stream.and_then(|s| handle(s, uid)) {
            eprintln!("afod: {e}");
        }
    }
    Ok(())
}

// Starts the daemon on the configured socket.
fn main() -> ExitCode {
    match socket_path().and_then(|p| serve(&p)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("afod: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks that the argument block splits on NUL and drops the trailing empty piece.
    #[test]
    fn test_split_args_nul_separated() {
        assert_eq!(split_args(b"16\0--json\0"), vec![b"16".to_vec(), b"--json".to_vec()]);
        assert!(split_args(b"").is_empty());
    }

    // Checks the default, parsed and clamped payload sizes.
    #[test]
    fn test_payload_len_default_and_clamp() {
        assert_eq!(payload_len(&[]), DEFAULT_PAYLOAD);
        assert_eq!(payload_len(&[b"4096".to_vec()]), 4096);
        assert_eq!(payload_len(&[b"0".to_vec()]), 1);
        assert_eq!(payload_len(&[b"99999999999".to_vec()]), MAX_PAYLOAD);
    }

    // Checks the payload ends with a newline and has the requested size.
    #[test]
    fn test_payload_shape() {
        assert_eq!(payload(5), b"xxxx\n");
    }

    // Checks that the relay path returns the payload followed by a zero status byte.
    #[test]
    fn test_answer_relay_returns_payload_and_status() {
        let (server, mut client) = UnixStream::pair().expect("socketpair");
        let req = Request { args: vec![b"3".to_vec()], fds: Vec::new() };
        answer(server, req).expect("answer");
        let mut got = Vec::new();
        client.read_to_end(&mut got).expect("read");
        assert_eq!(got, b"xx\n\0");
    }
}
