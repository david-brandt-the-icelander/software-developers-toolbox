// The daemon: listens on a unix socket, receives requests with passed descriptors and answers them.
use crate::cmd::{self, Ctx, Output};
use crate::engine::Engine;
use crate::proto::{self, Request};
use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_FDS: usize = 4;
const TICK_MS: i32 = 100;
const FIRST_READ: usize = 4096;
const CTL_WORDS: usize = 256;

// The reply to a connection a retiring daemon will not serve: a status byte the invoker reads first, then a tag that
// tells it apart from a command's own status, so the invoker reconnects to a new daemon and sends the request again.
pub const RESTART: &[u8] = b"\xfedaemon-restart";

// Daemon settings: the idle time before exiting, the binary to watch for an upgrade, and the socket path this process
// bound itself (none when launchd owns the socket), which it removes when it retires.
#[derive(Default)]
pub struct Config {
    pub idle: Duration,
    pub watch: Option<ExeWatch>,
    pub socket: Option<PathBuf>,
}

// The path whose binary starts the next daemon, and the identity of the binary this daemon runs, to notice when an
// upgrade has replaced it.
#[derive(Clone)]
pub struct ExeWatch {
    path: PathBuf,
    running: Option<(u64, u64, i64, i64, u64)>,
}

impl ExeWatch {
    // Watches `path` against the binary at `running` as it is now.
    pub fn new(path: PathBuf, running: &Path) -> ExeWatch {
        ExeWatch { running: identity(running), path }
    }

    // Tells whether the binary at the watched path is no longer the one this daemon runs.
    pub fn replaced(&self) -> bool {
        identity(&self.path) != self.running
    }
}

// Returns a file's device, inode, modification time and size, following links.
fn identity(p: &Path) -> Option<(u64, u64, i64, i64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).ok()?;
    Some((m.dev(), m.ino(), m.mtime(), m.mtime_nsec(), m.len()))
}

// The descriptors an invoker passes: its standard streams and working directory.
#[derive(Default)]
struct Fds {
    stdin: Option<OwnedFd>,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
    cwd: Option<OwnedFd>,
}

// Takes ownership of the descriptors in one control message; other message types yield none.
fn rights_in(c: *const libc::cmsghdr) -> Vec<OwnedFd> {
    // SAFETY: c points at a control message header inside a buffer filled by a successful recvmsg.
    unsafe {
        if (*c).cmsg_level != libc::SOL_SOCKET || (*c).cmsg_type != libc::SCM_RIGHTS {
            return vec![];
        }
        let data = libc::CMSG_DATA(c) as *const RawFd;
        let total: usize = (*c).cmsg_len as _;
        let bytes = total - libc::CMSG_LEN(0) as usize;
        (0..bytes / mem::size_of::<RawFd>())
            .map(|i| OwnedFd::from_raw_fd(data.add(i).read_unaligned()))
            .collect()
    }
}

// Collects the file descriptors carried by the control messages of a received message.
fn take_fds(msg: &libc::msghdr) -> Vec<OwnedFd> {
    let mut out = Vec::new();
    // SAFETY: msg was filled by a successful recvmsg, so the CMSG macros stay inside its control buffer.
    let mut c = unsafe { libc::CMSG_FIRSTHDR(msg) };
    while !c.is_null() {
        out.extend(rights_in(c));
        // SAFETY: c is a header returned by the CMSG macros for this msg.
        c = unsafe { libc::CMSG_NXTHDR(msg, c) };
    }
    out
}

// Receives bytes and any passed descriptors with one recvmsg call.
fn recv_with_fds(sock: RawFd, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    let mut ctl = [0u64; CTL_WORDS];
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
    let truncated = msg.msg_flags & libc::MSG_CTRUNC != 0;
    if truncated | (fds.len() > MAX_FDS) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too many descriptors"));
    }
    Ok((n as usize, fds))
}

// Reads one complete length-prefixed request and the descriptors that came with it.
fn read_request(stream: &UnixStream) -> io::Result<(Request, Fds)> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    let mut first = [0u8; FIRST_READ];
    let (n, fds) = recv_with_fds(stream.as_raw_fd(), &mut first)?;
    if n < 4 {
        return Err(bad("short request"));
    }
    let want = proto::frame_len([first[0], first[1], first[2], first[3]]).map_err(|m| bad(&m))?;
    let mut body = Vec::with_capacity(want);
    body.extend_from_slice(&first[4..n.min(4 + want)]);
    let mut reader = stream;
    let mut chunk = [0u8; FIRST_READ];
    while body.len() < want {
        let take = (want - body.len()).min(FIRST_READ);
        let got = reader.read(&mut chunk[..take])?;
        if got == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        body.extend_from_slice(&chunk[..got]);
    }
    let req = proto::decode(&body).map_err(|m| bad(&m))?;
    let mut it = fds.into_iter();
    Ok((req, Fds { stdin: it.next(), stdout: it.next(), stderr: it.next(), cwd: it.next() }))
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

// Resolves a directory descriptor to its path.
#[cfg(target_os = "linux")]
fn fd_path(fd: &OwnedFd) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

// Resolves a directory descriptor to its path.
#[cfg(not(target_os = "linux"))]
fn fd_path(fd: &OwnedFd) -> io::Result<PathBuf> {
    let mut buf = [0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX bytes into buf.
    let rc = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if rc == -1 {
        return Err(io::Error::last_os_error());
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Ok(PathBuf::from(String::from_utf8_lossy(&buf[..end]).into_owned()))
}

// Chooses the working directory from the request or the passed descriptor.
fn working_dir(req: &Request, fds: &Fds) -> io::Result<PathBuf> {
    if !req.cwd.is_empty() {
        return Ok(PathBuf::from(&req.cwd));
    }
    match &fds.cwd {
        Some(fd) => fd_path(fd),
        None => Err(io::Error::new(io::ErrorKind::InvalidData, "request has no working directory")),
    }
}

// Sends the output to the invoker's streams, or back over the socket when no streams were passed.
fn respond(stream: &mut UnixStream, out: &Output, fds: Fds) -> io::Result<()> {
    let Some(stdout) = fds.stdout else {
        stream.write_all(&[out.status])?;
        stream.write_all(out.text.as_bytes())?;
        return stream.write_all(out.notes.as_bytes());
    };
    let to_err = out.status != 0 && !out.raw;
    let (target, notes) = match (to_err, fds.stderr) {
        (true, Some(e)) => (e, None),
        (false, Some(e)) => (stdout, Some(e)),
        (_, None) => (stdout, None),
    };
    let mut f = File::from(target);
    let _ = f.write_all(out.text.as_bytes());
    if !out.raw {
        let _ = f.write_all(b"\n");
    }
    if !out.notes.is_empty() {
        match notes {
            Some(e) => {
                _ = File::from(e).write_all(format!("{}\n", out.notes.trim_start_matches('\n')).as_bytes())
            }
            None => _ = f.write_all(out.notes.as_bytes()),
        }
    }
    drop(f);
    stream.write_all(&[out.status])
}

// Serves one connection after checking the peer runs as the same user.
fn handle(stream: UnixStream, sh: &Shared) -> io::Result<()> {
    check_peer(&stream, sh.uid)?;
    let (req, fds) = read_request(&stream)?;
    if !long_running(&req.args) {
        return run_request(stream, &sh.engine, req, fds);
    }
    let (engine, active, last_ms, origin) =
        (sh.engine.clone(), sh.active.clone(), sh.last_ms.clone(), sh.origin);
    active.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        if let Err(e) = run_request(stream, &engine, req, fds) {
            eprintln!("daemon: {e}");
        }
        last_ms.store(now_ms(&origin), Ordering::SeqCst);
        active.fetch_sub(1, Ordering::SeqCst);
    });
    Ok(())
}

// Refuses peers running as another user.
fn check_peer(stream: &UnixStream, uid: u32) -> io::Result<()> {
    if peer_uid(stream)? != uid {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "peer uid mismatch"));
    }
    Ok(())
}

// True for requests that may run for a long time (investigate jobs and their lookups by run id, script calls), which get their own thread so they never hold a worker.
fn long_running(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "investigate")
        || args.first().is_some_and(|a| a == "zazz")
            && args.get(1).is_some_and(|a| matches!(a.as_str(), "call" | "run" | "exec-name"))
}

// Duplicates the invoker's three standard streams for a command that hands them to a script.
fn caller_of(stream: &UnixStream, fds: &Fds) -> Option<cmd::Caller> {
    let dup = |f: &Option<OwnedFd>| f.as_ref().and_then(|f| f.try_clone().ok());
    Some(cmd::Caller {
        stdio: [dup(&fds.stdin)?, dup(&fds.stdout)?, dup(&fds.stderr)?],
        conn: Some(stream.as_raw_fd()),
    })
}

// Runs a decoded request and answers it.
fn run_request(mut stream: UnixStream, engine: &Engine, req: Request, mut fds: Fds) -> io::Result<()> {
    let cwd = working_dir(&req, &fds)?;
    let caller = caller_of(&stream, &fds);
    let stdin: Option<Box<dyn Read>> = fds.stdin.take().map(|fd| Box::new(File::from(fd)) as Box<dyn Read>);
    let (session, roots) = crate::proto::split_session(&req.session);
    let ctx = Ctx { engine, cwd, session, roots, stdin: RefCell::new(stdin), caller: RefCell::new(caller) };
    let out = cmd::run(&ctx, &req.args);
    let quiet = matches!(req.args.first().map(String::as_str), None | Some("version" | "status"));
    if !quiet {
        crate::activity::record(serde_json::json!({
        "src": "zazzle",
        "cmd": req.args.first(),
        "session": ctx.session,
        "cwd": ctx.cwd,
        "status": out.status,
        }));
    }
    drop(ctx);
    respond(&mut stream, &out, fds)
}

// Waits up to the timeout for the listener to become readable.
fn wait_readable(fd: RawFd, timeout_ms: i32) -> bool {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: p is a live pollfd and the count is one.
    let rc = unsafe { libc::poll(&mut p, 1, timeout_ms) };
    rc > 0
}

// Milliseconds since the first call, used to track idleness without locks.
fn now_ms(origin: &Instant) -> u64 {
    origin.elapsed().as_millis() as u64
}

// Shared state the workers and the idle monitor agree on.
struct Shared {
    listener: UnixListener,
    engine: Arc<Engine>,
    stop: AtomicBool,
    active: Arc<AtomicUsize>,
    last_ms: Arc<AtomicU64>,
    origin: Instant,
    uid: u32,
    watch: Option<ExeWatch>,
    socket: Option<PathBuf>,
    retired: AtomicBool,
}

// Retires the daemon once its binary has been replaced: removes a socket it bound, so new connections start the new
// binary, and stops the workers.
fn retire_if_replaced(sh: &Shared) {
    if !sh.watch.as_ref().is_some_and(ExeWatch::replaced) || sh.retired.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(p) = &sh.socket {
        let _ = std::fs::remove_file(p);
    }
    sh.stop.store(true, Ordering::SeqCst);
}

// Answers every connection still queued on a retired daemon's own socket with RESTART.
fn restart_pending(sh: &Shared) {
    while let Ok((mut stream, _)) = sh.listener.accept() {
        let _ = stream.set_nonblocking(false).and_then(|()| stream.write_all(RESTART));
    }
}

// Reports an accept failure unless it only means another worker took the connection.
fn report_accept_error(e: &io::Error) {
    if e.kind() != io::ErrorKind::WouldBlock {
        eprintln!("daemon: accept: {e}");
    }
}

// One worker: waits for the listener, accepts directly and serves the connection.
fn worker(sh: &Shared, external_stop: &AtomicBool) {
    let stopped = || sh.stop.load(Ordering::SeqCst) || external_stop.load(Ordering::SeqCst);
    while !stopped() {
        if !wait_readable(sh.listener.as_raw_fd(), TICK_MS) {
            continue;
        }
        retire_if_replaced(sh);
        if sh.retired.load(Ordering::SeqCst) {
            break;
        }
        let stream = match sh.listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) => {
                report_accept_error(&e);
                continue;
            }
        };
        sh.active.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = stream.set_nonblocking(false).and_then(|()| handle(stream, sh)) {
            eprintln!("daemon: {e}");
        }
        sh.last_ms.store(now_ms(&sh.origin), Ordering::SeqCst);
        sh.active.fetch_sub(1, Ordering::SeqCst);
    }
}

// Writes pending cache records, reporting failures on stderr.
fn flush_quietly(engine: &Engine) {
    if let Err(e) = engine.flush() {
        eprintln!("daemon: cache write failed: {e}");
    }
}

// Answers connections on a pool of workers that accept directly, until stopped or idle.
pub fn serve(listener: UnixListener, engine: Arc<Engine>, cfg: &Config, stop: &AtomicBool) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, 8);
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let sh = Shared {
        listener,
        engine,
        stop: AtomicBool::new(false),
        active: Arc::new(AtomicUsize::new(0)),
        last_ms: Arc::new(AtomicU64::new(0)),
        origin: Instant::now(),
        uid,
        watch: cfg.watch.clone(),
        socket: cfg.socket.clone(),
        retired: AtomicBool::new(false),
    };
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| worker(&sh, stop));
        }
        let idle_ms = cfg.idle.as_millis() as u64;
        while !stop.load(Ordering::SeqCst) && !sh.stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(TICK_MS as u64));
            flush_quietly(&sh.engine);
            retire_if_replaced(&sh);
            let quiet = now_ms(&sh.origin).saturating_sub(sh.last_ms.load(Ordering::SeqCst));
            if sh.active.load(Ordering::SeqCst) == 0 && quiet >= idle_ms {
                break;
            }
        }
        sh.stop.store(true, Ordering::SeqCst);
    });
    flush_quietly(&sh.engine);
    // Under launchd the queued connections stay on launchd's socket, which starts the new binary once this one exits.
    if sh.retired.load(Ordering::SeqCst) && sh.socket.is_some() {
        restart_pending(&sh);
    }
    Ok(())
}

// Creates the private socket directory and clears a stale socket, refusing if a daemon already answers.
pub fn prepare(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path.parent().unwrap_or(Path::new(".")))?;
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::other("a daemon is already running"));
        }
        std::fs::remove_file(path)?;
    }
    Ok(())
}

// Binds a fresh listener at the path with owner-only permissions.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    prepare(path)?;
    // SAFETY: umask only changes this process's file-creation mask.
    let old = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(path);
    // SAFETY: restores the previous mask.
    unsafe { libc::umask(old) };
    listener
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod tests {
    use super::*;
    use crate::meta::Meta;
    use crate::proto::encode;
    use crate::testutil::{send_with_fds, TempDir};
    use std::os::fd::AsFd;
    use std::sync::atomic::AtomicBool;

    // A running test server and the pieces to talk to it.
    pub struct Running {
        pub sock: PathBuf,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<io::Result<()>>>,
        _dir: TempDir,
    }

    impl Running {
        // Starts a server on a temporary socket.
        pub fn start(idle: Duration) -> Running {
            Running::start_with(idle, Engine::new())
        }

        // Starts a server on its own socket with the given engine.
        pub fn start_with(idle: Duration, engine: Engine) -> Running {
            let dir = TempDir::new();
            let sock = dir.path().join("s");
            let listener = bind(&sock).expect("bind");
            let stop = Arc::new(AtomicBool::new(false));
            let (s2, engine) = (Arc::clone(&stop), Arc::new(engine));
            let handle = std::thread::spawn(move || {
                serve(listener, engine, &Config { idle, ..Config::default() }, &s2)
            });
            Running { sock, stop, handle: Some(handle), _dir: dir }
        }

        // Stops the server and returns its result.
        pub fn finish(mut self) -> io::Result<()> {
            self.stop.store(true, Ordering::SeqCst);
            self.handle.take().expect("handle").join().expect("join")
        }
    }

    // Makes a pipe and returns its read and write ends.
    fn pipe() -> (File, OwnedFd) {
        let mut fds = [0; 2];
        // SAFETY: fds has room for two descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: pipe returned two fresh descriptors we now own.
        unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    // Sends a request in fd mode and returns (stdout text, stderr text, status).
    fn fd_call(sock: &Path, cwd: &Path, args: &[&str], stdin_text: &[u8]) -> (String, String, u8) {
        fd_call_in(sock, cwd, "", args, stdin_text)
    }

    // Sends a request in fd mode under a session and returns (stdout text, stderr text, status).
    fn fd_call_in(
        sock: &Path,
        cwd: &Path,
        session: &str,
        args: &[&str],
        stdin_text: &[u8],
    ) -> (String, String, u8) {
        let (mut out_r, out_w) = pipe();
        let (mut err_r, err_w) = pipe();
        let (in_r, in_w) = pipe();
        let mut in_w = File::from(in_w);
        in_w.write_all(stdin_text).expect("stdin");
        drop(in_w);
        let dirfd = OwnedFd::from(File::open(cwd).expect("open cwd"));
        let req = Request {
            cwd: String::new(),
            session: session.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        };
        let mut s = UnixStream::connect(sock).expect("connect");
        let in_fd = OwnedFd::from(in_r);
        assert!(send_with_fds(
            &s,
            &encode(&req),
            &[in_fd.as_fd(), out_w.as_fd(), err_w.as_fd(), dirfd.as_fd()]
        ));
        drop((in_fd, out_w, err_w, dirfd));
        let mut status = [0u8; 1];
        s.read_exact(&mut status).expect("status");
        let (mut o, mut e) = (String::new(), String::new());
        out_r.read_to_string(&mut o).expect("stdout");
        err_r.read_to_string(&mut e).expect("stderr");
        (o, e, status[0])
    }

    // Checks fd mode: output goes to the passed stdout and the cwd comes from the descriptor.
    #[test]
    fn test_fd_mode_uses_passed_streams_and_cwd() {
        let work = TempDir::new();
        work.write("a.txt", b"needle\n");
        let srv = Running::start(Duration::from_secs(30));
        let (out, err, status) = fd_call(&srv.sock, work.path(), &["find", "needle"], b"");
        assert_eq!((status, err.as_str()), (0, ""));
        assert!(out.starts_with("a.txt\n1: needle\n-- 1 matches") && out.ends_with('\n'));
        srv.finish().expect("finish");
    }

    // Checks a read's source goes to the passed stdout and the rules that apply to it to stderr, so a pipe gets only the source; over the socket both come back.
    #[test]
    fn test_rules_go_to_stderr_not_the_pipe() {
        let work = TempDir::new();
        std::fs::create_dir_all(work.path().join(".git")).expect("git");
        work.write("sub/AGENTS.md", b"sub rule\n");
        work.write("sub/a.txt", b"source line\n");
        let srv = Running::start(Duration::from_secs(30));
        let (out, err, status) = fd_call_in(&srv.sock, work.path(), "s1", &["read", "sub/a.txt"], b"");
        assert_eq!(status, 0);
        assert!(out.contains("source line") && !out.contains("sub rule"), "{out}");
        assert!(err.starts_with("[instructions for this path: sub/AGENTS.md]\nsub rule"), "{err}");
        work.write("sub/AGENTS.md", b"sub rule changed\n");
        let req = Request {
            cwd: work.path().display().to_string(),
            session: "s1".into(),
            args: vec!["read".into(), "sub/a.txt".into(), "--disable-session-deduping".into()],
        };
        let mut s = UnixStream::connect(&srv.sock).expect("connect");
        let (_, text) = proto::roundtrip(&mut s, &req).expect("roundtrip");
        assert!(text.contains("source line") && text.contains("sub rule changed"), "{text}");
        srv.finish().expect("finish");
    }

    // Checks errors go to the passed stderr with a nonzero status.
    #[test]
    fn test_fd_mode_error_goes_to_stderr() {
        let work = TempDir::new();
        let srv = Running::start(Duration::from_secs(30));
        let (out, err, status) = fd_call(&srv.sock, work.path(), &["bogus"], b"");
        assert_eq!((out.as_str(), status), ("", 2));
        assert!(err.starts_with("error: unknown command"));
        srv.finish().expect("finish");
    }

    // Checks relay mode with an explicit cwd returns status then text.
    #[test]
    fn test_relay_mode_roundtrip() {
        let work = TempDir::new();
        work.write("a.txt", b"x\n");
        let srv = Running::start(Duration::from_secs(30));
        let req =
            Request { cwd: work.path().display().to_string(), session: "s".into(), args: vec!["ls".into()] };
        let mut s = UnixStream::connect(&srv.sock).expect("connect");
        let (status, text) = proto::roundtrip(&mut s, &req).expect("roundtrip");
        assert_eq!(status, 0);
        assert!(text.contains("a.txt 1"));
        srv.finish().expect("finish");
    }

    // Checks an investigate job outlives its caller: a client that sends the job and disconnects loses nothing, a fresh client submitting the same run id gets the one result without a second model call, and after a daemon restart the same result and its usage come back by id alone.
    #[test]
    fn test_investigate_kept_across_disconnect_and_restart() {
        let _g = crate::testutil::env_lock();
        let t = TempDir::new();
        let asked = t.path().join("asked");
        let worker = t.write("ai-agent", format!("#!/bin/sh\necho '{{\"ready\":\"m\"}}'\nwhile read l; do echo x >> {}; sleep 0.4; printf '%s\\n' '{{\"text\":\"ANSWER\\nFINDING a.txt:1 the needle is here\",\"tokens\":7}}'; done\n", asked.display()).as_bytes());
        std::fs::set_permissions(&worker, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        std::env::set_var("ZAZZLE_LLM", &worker);
        crate::llm::reap(Duration::ZERO);
        let work = TempDir::new();
        work.write("a.txt", b"needle\n");
        let runs = t.path().join("runs");
        let req = |args: &[&str]| Request {
            cwd: work.path().display().to_string(),
            session: "s".into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        let job = ["investigate", "where is the needle?", "--path", ".", "--run-id", "t1", "--seconds", "20"];
        let srv = Running::start_with(Duration::from_secs(30), Engine::new().with_runs(Some(runs.clone())));
        UnixStream::connect(&srv.sock)
            .expect("connect")
            .write_all(&encode(&req(&job)))
            .expect("send and leave");
        let (status, first) =
            proto::roundtrip(&mut UnixStream::connect(&srv.sock).expect("connect"), &req(&job))
                .expect("again");
        assert_eq!(status, 0, "{first}");
        assert!(first.contains("the needle is here") && first.contains("a.txt:1"), "{first}");
        srv.finish().expect("finish");
        let srv = Running::start_with(Duration::from_secs(30), Engine::new().with_runs(Some(runs.clone())));
        let (status, later) = proto::roundtrip(
            &mut UnixStream::connect(&srv.sock).expect("connect"),
            &req(&["investigate", "--run-id", "t1"]),
        )
        .expect("lookup");
        let (out, err, code) =
            fd_call(&srv.sock, work.path(), &["investigate", "where?", "--path", ".", "--run-id", "t3"], b"");
        srv.finish().expect("finish");
        assert_eq!(code, 0, "{out}{err}");
        assert!(
            err.starts_with(
                "investigate: run t3 (if this wait is cut off: zazzle investigate --run-id t3)\n"
            ),
            "the id comes before the wait: {err}"
        );
        std::env::remove_var("ZAZZLE_LLM");
        crate::llm::reap(Duration::ZERO);
        assert_eq!(status, 0);
        let body = |s: &str| s.split("\n-- the kept result").next().unwrap_or_default().to_string();
        assert_eq!(body(&later), body(&first), "the same result after the restart");
        assert!(later.contains("the kept result of investigate run t1; no model was asked again"), "{later}");
        assert_eq!(
            std::fs::read_to_string(&asked).expect("asked").lines().count(),
            2,
            "one model call for every submission of t1, one for t3"
        );
        let kept: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(runs.join("t1/result.json")).expect("result"))
                .expect("json");
        assert_eq!(
            (kept["usage"]["calls"].as_u64(), kept["usage"]["output_tokens"].as_u64()),
            (Some(1), Some(7)),
            "{kept}"
        );
    }

    // Checks `investigate --run-id ID --cancel` from another connection stops a running job at once: its local model wait ends, both callers get its result naming the cancel, and that result is kept.
    #[test]
    fn test_investigate_cancel_from_another_client() {
        let _g = crate::testutil::env_lock();
        let t = TempDir::new();
        let worker = t.write("ai-agent", b"#!/bin/sh\necho '{\"ready\":\"m\"}'\nwhile read l; do sleep 5; echo '{\"text\":\"late\"}'; done\n");
        std::fs::set_permissions(&worker, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        std::env::set_var("ZAZZLE_LLM", &worker);
        crate::llm::reap(Duration::ZERO);
        let work = TempDir::new();
        work.write("a.txt", b"needle\n");
        let runs = t.path().join("runs");
        let req = |args: &[&str]| Request {
            cwd: work.path().display().to_string(),
            session: "s".into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        let srv = Running::start_with(Duration::from_secs(30), Engine::new().with_runs(Some(runs.clone())));
        let sock = srv.sock.clone();
        let job = req(&["investigate", "where?", "--path", ".", "--run-id", "c1", "--seconds", "60"]);
        let t0 = Instant::now();
        let first = std::thread::spawn(move || {
            proto::roundtrip(&mut UnixStream::connect(&sock).expect("connect"), &job).expect("job")
        });
        std::thread::sleep(Duration::from_millis(400));
        let (_, cancelled) = proto::roundtrip(
            &mut UnixStream::connect(&srv.sock).expect("connect"),
            &req(&["investigate", "--run-id", "c1", "--cancel"]),
        )
        .expect("cancel");
        let (_, theirs) = first.join().expect("first");
        srv.finish().expect("finish");
        std::env::remove_var("ZAZZLE_LLM");
        crate::llm::reap(Duration::ZERO);
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "stopped at the cancel, not the 5 s answer: {:?}",
            t0.elapsed()
        );
        assert!(cancelled.contains("the run was cancelled while the local model worked"), "{cancelled}");
        assert_eq!(theirs, cancelled, "both callers get the one result");
        let kept: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(runs.join("c1/result.json")).expect("result"))
                .expect("json");
        assert_eq!(kept["state"], "cancelled");
    }

    // Checks bad requests are rejected without killing the server.
    #[test]
    fn test_bad_requests_are_survivable() {
        let srv = Running::start(Duration::from_secs(30));
        for bytes in
            [vec![1u8, 0], vec![2, 0, 0, 0, 1, 1], ((proto::MAX_REQUEST + 1) as u32).to_ne_bytes().to_vec()]
        {
            let mut s = UnixStream::connect(&srv.sock).expect("connect");
            s.write_all(&bytes).expect("write");
            drop(s.shutdown(std::net::Shutdown::Write));
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
        }
        let mut s = UnixStream::connect(&srv.sock).expect("connect");
        let req = Request { cwd: String::new(), session: String::new(), args: vec![] };
        s.write_all(&encode(&req)).expect("write");
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        let req = Request { cwd: "/".into(), session: String::new(), args: vec!["version".into()] };
        let mut s = UnixStream::connect(&srv.sock).expect("connect");
        assert_eq!(proto::roundtrip(&mut s, &req).expect("still alive").0, 0);
        srv.finish().expect("finish");
    }

    // Replaces a file the way an upgrade does: a new file renamed over the old path.
    fn replace(path: &Path, text: &str) {
        let tmp = path.with_extension("new");
        std::fs::write(&tmp, text).expect("write");
        std::fs::rename(&tmp, path).expect("rename");
    }

    // Checks the watch notices a binary renamed over its path or removed, and not an untouched one.
    #[test]
    fn test_exe_watch_replaced() {
        let dir = TempDir::new();
        let exe = dir.path().join("daemon");
        std::fs::write(&exe, "v1").expect("write");
        let watch = ExeWatch::new(exe.clone(), &exe);
        assert!(!watch.replaced());
        replace(&exe, "v2");
        assert!(watch.replaced());
        assert!(!ExeWatch::new(exe.clone(), &exe).replaced());
        std::fs::remove_file(&exe).expect("remove");
        assert!(ExeWatch::new(exe.clone(), &exe).running.is_none());
        assert!(!ExeWatch::new(exe.clone(), &exe).replaced());
    }

    // Starts serving with a binary already replaced and one connection queued, and returns that connection.
    fn serve_replaced(own_socket: bool) -> (UnixStream, PathBuf, TempDir) {
        let dir = TempDir::new();
        let (sock, exe) = (dir.path().join("s"), dir.path().join("daemon"));
        std::fs::write(&exe, "v1").expect("write");
        let watch = ExeWatch::new(exe.clone(), &exe);
        replace(&exe, "v2");
        let listener = bind(&sock).expect("bind");
        let queued = UnixStream::connect(&sock).expect("connect");
        let cfg = Config {
            idle: Duration::from_secs(30),
            watch: Some(watch),
            socket: own_socket.then(|| sock.clone()),
        };
        serve(listener, Arc::new(Engine::new()), &cfg, &AtomicBool::new(false)).expect("serve");
        (queued, sock, dir)
    }

    // Checks a daemon whose own socket's binary was replaced removes the socket, answers queued work with RESTART
    // and exits.
    #[test]
    fn test_replaced_binary_restarts_own_socket() {
        let (mut queued, sock, _dir) = serve_replaced(true);
        assert!(!sock.exists());
        let mut reply = Vec::new();
        queued.read_to_end(&mut reply).expect("read");
        assert_eq!(reply, RESTART);
    }

    // Checks a daemon on launchd's socket exits without answering, leaving queued work for the new binary launchd starts.
    #[test]
    fn test_replaced_binary_leaves_launchd_socket() {
        let (mut queued, sock, _dir) = serve_replaced(false);
        assert!(sock.exists());
        let mut reply = Vec::new();
        let _ = queued.read_to_end(&mut reply);
        assert!(reply.is_empty());
    }

    // Checks the server exits on its own once idle.
    #[test]
    fn test_idle_exit() {
        let srv = Running::start(Duration::from_millis(150));
        let start = Instant::now();
        while !srv.handle.as_ref().expect("handle").is_finished() && start.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(srv.handle.as_ref().expect("handle").is_finished());
        srv.finish().expect("finish");
    }

    // Checks prepare refuses a live daemon and clears a stale socket.
    #[test]
    fn test_prepare_live_and_stale() {
        let srv = Running::start(Duration::from_secs(30));
        assert_eq!(prepare(&srv.sock).unwrap_err().to_string(), "a daemon is already running");
        let dir = TempDir::new();
        let stale = dir.path().join("sub/s");
        drop(bind(&stale).expect("first bind"));
        assert!(stale.exists());
        prepare(&stale).expect("stale cleared");
        assert!(!stale.exists());
        srv.finish().expect("finish");
    }

    // Checks the socket is owner-only.
    #[test]
    fn test_socket_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let p = dir.path().join("d/s");
        let _l = bind(&p).expect("bind");
        assert_eq!(std::fs::metadata(&p).expect("meta").permissions().mode() & 0o077, 0);
        assert_eq!(
            std::fs::metadata(p.parent().expect("dir")).expect("meta").permissions().mode() & 0o077,
            0
        );
    }

    // Checks a request without cwd or descriptors is rejected.
    #[test]
    fn test_working_dir_missing() {
        let req = Request { cwd: String::new(), session: String::new(), args: vec![] };
        assert!(working_dir(&req, &Fds::default()).is_err());
        let req = Request { cwd: "/x".into(), ..req };
        assert_eq!(working_dir(&req, &Fds::default()).expect("cwd"), PathBuf::from("/x"));
    }

    // Checks wait_readable reports quiet and ready sockets.
    #[test]
    fn test_wait_readable() {
        let (a, mut b) = UnixStream::pair().expect("pair");
        assert!(!wait_readable(a.as_raw_fd(), 1));
        b.write_all(b"x").expect("write");
        assert!(wait_readable(a.as_raw_fd(), 1000));
        assert!(!wait_readable(-1, 1));
    }

    // Checks a failing cache write is reported without stopping anything.
    #[test]
    fn test_flush_failure_is_reported_not_fatal() {
        let dir = TempDir::new();
        let store = crate::store::Store::open(dir.path()).expect("store");
        store.put(
            Path::new("/f"),
            crate::store::StatKey { dev: 1, ino: 1, size: 1, mtime_ns: 1 },
            Meta::compute(b"x").info(),
        );
        store.break_log();
        let engine = Engine::with_store(store);
        flush_quietly(&engine);
        assert_eq!(engine.stored_records(), 1);
    }

    // Checks accept errors are reported except the benign would-block case.
    #[test]
    fn test_report_accept_error() {
        report_accept_error(&io::Error::from(io::ErrorKind::WouldBlock));
        report_accept_error(&io::Error::other("emfile"));
    }

    // Checks control messages of other types yield no descriptors.
    #[test]
    fn test_rights_in_ignores_other_messages() {
        // SAFETY: cmsghdr is plain data for which all-zero is valid.
        let mut hdr: libc::cmsghdr = unsafe { mem::zeroed() };
        hdr.cmsg_level = libc::SOL_SOCKET;
        hdr.cmsg_type = libc::SCM_RIGHTS + 50;
        assert!(rights_in(&hdr).is_empty());
        hdr.cmsg_level = libc::SOL_SOCKET + 50;
        hdr.cmsg_type = libc::SCM_RIGHTS;
        assert!(rights_in(&hdr).is_empty());
    }

    // Checks recvmsg failures are reported.
    #[test]
    fn test_recv_error_on_bad_descriptor() {
        let mut buf = [0u8; 8];
        assert!(recv_with_fds(-1, &mut buf).is_err());
    }

    // Checks requests carrying too many descriptors, or truncated control data, are rejected.
    #[test]
    fn test_too_many_descriptors_rejected() {
        let srv = Running::start(Duration::from_secs(30));
        let null = File::open("/dev/null").expect("null");
        for count in [5usize, 40] {
            let mut s = UnixStream::connect(&srv.sock).expect("connect");
            let fds: Vec<_> = (0..count).map(|_| null.as_fd()).collect();
            let req = Request { cwd: "/".into(), session: String::new(), args: vec!["version".into()] };
            if !send_with_fds(&s, &encode(&req), &fds) {
                continue;
            }
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            assert!(buf.is_empty(), "{count} descriptors must not get an answer");
        }
        srv.finish().expect("finish");
    }

    // Checks a request split across several packets is reassembled.
    #[test]
    fn test_partial_request_is_reassembled() {
        let srv = Running::start(Duration::from_secs(30));
        let req = Request { cwd: "/".into(), session: String::new(), args: vec!["version".into()] };
        let bytes = encode(&req);
        let mut s = UnixStream::connect(&srv.sock).expect("connect");
        s.write_all(&bytes[..6]).expect("first part");
        std::thread::sleep(Duration::from_millis(60));
        s.write_all(&bytes[6..]).expect("rest");
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).expect("read");
        assert_eq!(buf[0], 0);
        assert!(String::from_utf8_lossy(&buf[1..]).starts_with("zazzle "));
        srv.finish().expect("finish");
    }

    // Checks a request cut off mid-way is dropped and the server survives.
    #[test]
    fn test_truncated_request_is_dropped() {
        let srv = Running::start(Duration::from_secs(30));
        let req = Request { cwd: "/".into(), session: String::new(), args: vec!["version".into()] };
        let bytes = encode(&req);
        let mut s = UnixStream::connect(&srv.sock).expect("connect");
        s.write_all(&bytes[..6]).expect("part");
        drop(s);
        let mut s = UnixStream::connect(&srv.sock).expect("connect");
        assert_eq!(proto::roundtrip(&mut s, &req).expect("alive").0, 0);
        srv.finish().expect("finish");
    }

    // Checks peer_uid fails on something that is not a socket.
    #[test]
    fn test_peer_uid_fails_on_non_socket() {
        let (r, _w) = pipe();
        let fake = UnixStream::from(OwnedFd::from(r));
        assert!(peer_uid(&fake).is_err());
    }

    // Checks a peer with another uid is refused.
    #[test]
    fn test_handle_rejects_other_uid() {
        let (a, _b) = UnixStream::pair().expect("pair");
        // SAFETY: getuid has no preconditions.
        let me = unsafe { libc::getuid() };
        let err = check_peer(&a, me + 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    // Checks raw outputs go to stdout as they are, whatever the status.
    #[test]
    fn test_respond_raw() {
        let (mut a, mut b) = UnixStream::pair().expect("pair");
        let (out_w, mut out_r) = UnixStream::pair().expect("out");
        let fds = Fds { stdout: Some(OwnedFd::from(out_w)), ..Fds::default() };
        respond(&mut a, &Output::raw("plain".into(), 3), fds).expect("respond");
        drop(a);
        let mut status = Vec::new();
        b.read_to_end(&mut status).expect("status");
        let mut text = String::new();
        out_r.read_to_string(&mut text).expect("text");
        assert_eq!((status, text.as_str()), (vec![3], "plain"));
    }

    // Checks a script call whose working directory cannot be found fails on its own thread without an answer.
    #[test]
    fn test_long_running_request_error() {
        let srv = Running::start(Duration::from_secs(30));
        let req =
            Request { cwd: String::new(), session: String::new(), args: vec!["zazz".into(), "call".into()] };
        assert!(crate::client::call(&srv.sock, &req).is_err());
        srv.finish().expect("stop");
    }

    // Checks script calls run on their own thread with the caller's streams and still answer.
    #[test]
    fn test_long_running_requests_get_a_thread() {
        let srv = Running::start(Duration::from_secs(30));
        let t = TempDir::new();
        let (out, err, status) = fd_call(&srv.sock, t.path(), &["zazz", "call"], b"");
        assert_eq!((out.as_str(), status), ("", 2));
        assert!(err.contains("\"code\":\"usage\""), "{err}");
        assert!(long_running(&["zazz".into(), "run".into()]) && !long_running(&["zazz".into(), "ls".into()]));
        assert!(!long_running(&["read".into()]));
        srv.finish().expect("stop");
    }

    // Checks an open connection keeps an otherwise idle server alive until it finishes.
    #[test]
    fn test_active_connection_defers_idle_exit() {
        let srv = Running::start(Duration::from_millis(150));
        let held = UnixStream::connect(&srv.sock).expect("connect");
        std::thread::sleep(Duration::from_millis(600));
        assert!(!srv.handle.as_ref().expect("handle").is_finished());
        drop(held);
        let start = Instant::now();
        while !srv.handle.as_ref().expect("handle").is_finished() && start.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(srv.handle.as_ref().expect("handle").is_finished());
        srv.finish().expect("finish");
    }
}
