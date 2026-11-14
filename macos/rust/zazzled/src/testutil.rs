// Test helpers: unique temporary directories that clean up after themselves.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

// A temporary directory removed when dropped.
pub struct TempDir(PathBuf);

impl Default for TempDir {
    // Creates a fresh empty directory.
    fn default() -> TempDir {
        TempDir::new()
    }
}

impl TempDir {
    // Creates a fresh empty directory under the system temp dir.
    pub fn new() -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("zazzled-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir.canonicalize().expect("canonicalize temp dir"))
    }

    // Returns the directory path.
    pub fn path(&self) -> &Path {
        &self.0
    }

    // Writes a file (creating parent directories) and returns its path.
    pub fn write(&self, rel: &str, content: &[u8]) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(&p, content).expect("write file");
        p
    }
}

impl Drop for TempDir {
    // Removes the directory tree.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Runs a command line in a directory with a fresh engine and no session.
pub fn run_cmd(cwd: &Path, args: &[&str]) -> crate::cmd::Output {
    run_cmd_with(&crate::engine::Engine::new(), cwd, "", args)
}

// Runs a command line with a shared engine and a session key.
pub fn run_cmd_with(
    engine: &crate::engine::Engine,
    cwd: &Path,
    session: &str,
    args: &[&str],
) -> crate::cmd::Output {
    let ctx = crate::cmd::Ctx {
        engine,
        cwd: cwd.to_path_buf(),
        session: session.to_string(),
        stdin: std::cell::RefCell::new(None),
        caller: std::cell::RefCell::new(None),
    };
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    crate::cmd::run(&ctx, &args)
}

// Runs a command line with a shared engine and the given standard input.
pub fn run_cmd_stdin(
    engine: &crate::engine::Engine,
    cwd: &Path,
    args: &[&str],
    stdin: &'static [u8],
) -> crate::cmd::Output {
    let ctx = crate::cmd::Ctx {
        engine,
        cwd: cwd.to_path_buf(),
        session: String::new(),
        stdin: std::cell::RefCell::new(Some(Box::new(stdin))),
        caller: std::cell::RefCell::new(None),
    };
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    crate::cmd::run(&ctx, &args)
}

// Sends bytes plus file descriptors over a unix socket with SCM_RIGHTS.
pub fn send_with_fds(
    sock: &std::os::unix::net::UnixStream,
    bytes: &[u8],
    fds: &[std::os::fd::BorrowedFd],
) -> bool {
    use std::os::fd::AsRawFd;
    let raw: Vec<i32> = fds.iter().map(|f| f.as_raw_fd()).collect();
    let space = unsafe { libc::CMSG_SPACE((raw.len() * 4) as u32) } as usize;
    let mut ctl = vec![0u64; space.div_ceil(8)];
    let mut iov = libc::iovec { iov_base: bytes.as_ptr() as *mut _, iov_len: bytes.len() };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = ctl.as_mut_ptr().cast();
    msg.msg_controllen = space as _;
    // SAFETY: the control buffer is sized by CMSG_SPACE for the descriptors copied in.
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN((raw.len() * 4) as u32) as _;
        std::ptr::copy_nonoverlapping(raw.as_ptr(), libc::CMSG_DATA(c).cast(), raw.len());
        libc::sendmsg(sock.as_raw_fd(), &msg, 0) >= 0
    }
}

static WORKER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

// Builds zazz-worker (release) once per test run and returns its path.
pub fn worker_bin() -> PathBuf {
    WORKER
        .get_or_init(|| {
            let ws = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace").to_path_buf();
            let target = ws.join("target");
            let mut cmd = std::process::Command::new(env!("CARGO"));
            cmd.current_dir(&ws)
                .args(["build", "--release", "-p", "zazz-worker", "--target-dir"])
                .arg(&target);
            for k in [
                "RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_TARGET_DIR",
                "LLVM_PROFILE_FILE",
                "CARGO_LLVM_COV",
            ] {
                cmd.env_remove(k);
            }
            let st = cmd.status().expect("run cargo build for zazz-worker");
            assert!(st.success(), "building zazz-worker failed");
            target.join("release/zazz-worker")
        })
        .clone()
}

static LUA_MODULE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

// Builds the Lua language module (release) once per test run, in its own target directory, and returns its path.
pub fn lua_module() -> PathBuf {
    LUA_MODULE
        .get_or_init(|| {
            let ws = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace").to_path_buf();
            let target = ws.join("target/zazzle-lang");
            let mut cmd = std::process::Command::new(env!("CARGO"));
            cmd.current_dir(&ws)
                .args(["build", "--release", "-p", "zazzle-lang", "--features", "lua", "--target-dir"])
                .arg(&target);
            for k in [
                "RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_TARGET_DIR",
                "LLVM_PROFILE_FILE",
                "CARGO_LLVM_COV",
            ] {
                cmd.env_remove(k);
            }
            let st = cmd.status().expect("run cargo build for zazzle-lang");
            assert!(st.success(), "building the Lua module failed");
            let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
            target.join(format!("release/libzazzle_lang.{ext}"))
        })
        .clone()
}

// An engine whose zazz registry is in a temporary directory and whose worker is the freshly built one.
pub fn zazz_engine(t: &TempDir) -> crate::engine::Engine {
    let e = crate::engine::Engine::new();
    e.zazz.set_root(&t.path().join("zazz-store"));
    e.zazz.set_worker(&worker_bin());
    e
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// Serialises tests that change process environment variables.
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// Backdates a file's mtime by an hour so caches will trust it.
pub fn age(path: &Path) {
    let f = std::fs::File::open(path).expect("open for age");
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600)).expect("set mtime");
}
