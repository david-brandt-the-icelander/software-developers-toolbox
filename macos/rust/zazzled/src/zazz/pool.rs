// Warm script workers: spawned on demand per (script, directory, policy), reused for calls, recycled when idle.
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use zazz::proto;

const IDLE: Duration = Duration::from_secs(60);
const MAX_CALLS: u32 = 1000;
const MAX_IDLE_PER_KEY: usize = 2;
const MAX_IDLE_TOTAL: usize = 32;
const START_TIMEOUT: Duration = Duration::from_secs(20);
const REAP_EVERY: Duration = Duration::from_secs(5);

// How to start a worker for one pool key.
#[derive(Debug, Clone)]
pub struct Spec {
    pub key: String,
    pub program: Vec<String>,
    pub module: Option<PathBuf>,
    pub cwd: PathBuf,
    pub hello: Value,
    pub sandbox: Option<super::sandbox::Policy>,
}

// A running worker process and its control socket.
#[derive(Debug)]
pub struct Worker {
    child: Child,
    pub sock: UnixStream,
    pub calls: u32,
    idle_since: Instant,
}

impl Worker {
    // Wraps a process and socket as a worker, for pool tests.
    #[cfg(test)]
    pub fn for_tests(child: Child, sock: UnixStream) -> Worker {
        Worker { child, sock, calls: 0, idle_since: Instant::now() }
    }

    // Kills the process and reaps it.
    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// Idle workers by key.
#[derive(Default)]
pub struct Pool {
    idle: Mutex<HashMap<String, Vec<Worker>>>,
    reaper: Mutex<bool>,
}

// Moves a descriptor to a fixed number in a child before exec, clearing close-on-exec.
fn move_fd(src: RawFd, dst: RawFd) -> io::Result<()> {
    // SAFETY: plain descriptor syscalls between fork and exec on descriptors this process owns.
    let rc = unsafe {
        if src == dst {
            libc::fcntl(dst, libc::F_SETFD, 0)
        } else {
            libc::dup2(src, dst)
        }
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// Reads one frame with a deadline.
fn read_frame_by(sock: &mut UnixStream, deadline: Duration) -> io::Result<Option<zazz::Json>> {
    sock.set_read_timeout(Some(deadline))?;
    let r = proto::read_frame(sock);
    sock.set_read_timeout(None)?;
    r
}

// Converts between the SDK's JSON and serde_json through text.
pub fn to_sdk(v: &Value) -> zazz::Json {
    zazz::Json::parse(&v.to_string()).expect("serde_json output is valid JSON")
}

// Converts SDK JSON to serde_json.
pub fn from_sdk(v: &zazz::Json) -> Value {
    serde_json::from_str(&v.to_string()).expect("SDK output is valid JSON")
}

// Starts a worker: control socket on fd 3, module on fd 4, a new session, no core dumps, then says hello.
pub fn spawn(spec: &Spec) -> Result<(Worker, Value), String> {
    let (ours, theirs) = UnixStream::pair().map_err(|e| e.to_string())?;
    let module =
        spec.module.as_ref().map(File::open).transpose().map_err(|e| format!("opening the module: {e}"))?;
    let (sock_fd, module_fd) = (theirs.as_raw_fd(), module.as_ref().map(AsRawFd::as_raw_fd));
    let (prog, args) = spec.program.split_first().ok_or("no program to start")?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .current_dir(&spec.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the closure only calls async-signal-safe syscalls on descriptors owned by this process.
    unsafe {
        cmd.pre_exec(move || {
            move_fd(sock_fd, 3)?;
            if let Some(m) = module_fd {
                move_fd(m, 4)?;
            }
            libc::setsid();
            let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            libc::setrlimit(libc::RLIMIT_CORE, &none);
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|e| format!("starting {prog}: {e}"))?;
    drop((theirs, module));
    let mut w = Worker { child, sock: ours, calls: 0, idle_since: Instant::now() };
    let hello = proto::write_frame(&mut w.sock, &to_sdk(&spec.hello))
        .and_then(|()| read_frame_by(&mut w.sock, START_TIMEOUT));
    match hello {
        Ok(Some(r)) if proto::kind(&r) == "ready" => Ok((w, from_sdk(&r))),
        Ok(Some(r)) => {
            w.kill();
            Err(format!(
                "the script failed to start: {}",
                r["error"]["message"].str().unwrap_or("no ready message")
            ))
        }
        Ok(None) | Err(_) => {
            w.kill();
            Err("the script worker did not answer its first message".into())
        }
    }
}

impl Pool {
    // Takes an idle worker for the key, or starts one.
    pub fn acquire(self: &Arc<Pool>, spec: &Spec) -> Result<Worker, String> {
        self.start_reaper();
        if let Some(w) = self.idle.lock().expect("pool lock").get_mut(&spec.key).and_then(Vec::pop) {
            return Ok(w);
        }
        spawn(spec).map(|(w, _)| w)
    }

    // Returns a worker after a call; unhealthy, overused or surplus workers are killed.
    pub fn release(&self, key: &str, mut w: Worker, healthy: bool) {
        w.calls += 1;
        w.idle_since = Instant::now();
        let mut idle = self.idle.lock().expect("pool lock");
        let total: usize = idle.values().map(Vec::len).sum();
        let list = idle.entry(key.to_string()).or_default();
        if !healthy || w.calls >= MAX_CALLS || list.len() >= MAX_IDLE_PER_KEY || total >= MAX_IDLE_TOTAL {
            drop(idle);
            w.kill();
            return;
        }
        list.push(w);
    }

    // Kills workers idle for longer than the limit; returns how many.
    pub fn reap(&self, idle_for: Duration) -> usize {
        let mut gone = Vec::new();
        let mut idle = self.idle.lock().expect("pool lock");
        for list in idle.values_mut() {
            let (old, keep): (Vec<Worker>, Vec<Worker>) =
                list.drain(..).partition(|w| w.idle_since.elapsed() >= idle_for);
            *list = keep;
            gone.extend(old);
        }
        idle.retain(|_, l| !l.is_empty());
        drop(idle);
        let n = gone.len();
        gone.into_iter().for_each(Worker::kill);
        n
    }

    // Kills every idle worker.
    pub fn shutdown(&self) -> usize {
        self.reap(Duration::ZERO)
    }

    // The number of idle workers.
    pub fn idle_count(&self) -> usize {
        self.idle.lock().expect("pool lock").values().map(Vec::len).sum()
    }

    // Starts the background reaper once; it stops when the pool is dropped.
    fn start_reaper(self: &Arc<Pool>) {
        let mut started = self.reaper.lock().expect("reaper lock");
        if *started {
            return;
        }
        *started = true;
        let weak: Weak<Pool> = Arc::downgrade(self);
        std::thread::spawn(move || loop {
            std::thread::sleep(REAP_EVERY);
            let Some(pool) = weak.upgrade() else { return };
            pool.reap(IDLE);
        });
    }
}

// The path of the worker module for a key's script.
pub fn module_path(dir: &Path, hex: &str) -> PathBuf {
    dir.join(format!("{hex}.cwasm"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // A fake worker: a shell script cannot speak frames, so it just exits.
    fn spec(program: &[&str], cwd: &Path) -> Spec {
        Spec {
            key: "k".into(),
            program: program.iter().map(|s| s.to_string()).collect(),
            module: None,
            cwd: cwd.to_path_buf(),
            hello: serde_json::json!({"t": "hello"}),
            sandbox: None,
        }
    }

    // Checks spawn errors: missing program, no program, a worker that never answers, a missing module.
    #[test]
    fn test_spawn_errors() {
        let t = TempDir::new();
        assert!(spawn(&spec(&["/nonexistent/worker"], t.path())).unwrap_err().contains("starting"));
        assert!(spawn(&spec(&[], t.path())).unwrap_err().contains("no program"));
        assert!(spawn(&spec(&["true"], t.path())).unwrap_err().contains("did not answer"));
        let mut s = spec(&["true"], t.path());
        s.module = Some(t.path().join("missing.cwasm"));
        assert!(spawn(&s).unwrap_err().contains("opening the module"));
    }

    // Checks the descriptor mover, including moving a descriptor onto itself.
    #[test]
    fn test_move_fd() {
        let f = File::open("/dev/null").expect("open");
        let fd = f.as_raw_fd();
        assert!(move_fd(fd, fd).is_ok());
        assert!(move_fd(-1, 1000).is_err());
        assert!(move_fd(fd, -1).is_err());
    }

    // Checks release keeps at most MAX_IDLE_PER_KEY per key and MAX_IDLE_TOTAL overall, and kills unhealthy or overused workers.
    #[test]
    fn test_release_limits() {
        let fake = || {
            let (a, _b) = UnixStream::pair().expect("pair");
            Worker::for_tests(Command::new("sleep").arg("30").spawn().expect("sleep"), a)
        };
        let pool = Pool::default();
        pool.release("a", fake(), false);
        let mut old = fake();
        old.calls = MAX_CALLS;
        pool.release("a", old, true);
        assert_eq!(pool.idle_count(), 0);
        for _ in 0..=MAX_IDLE_PER_KEY {
            pool.release("a", fake(), true);
        }
        assert_eq!(pool.idle_count(), MAX_IDLE_PER_KEY);
        for i in 0..MAX_IDLE_TOTAL {
            pool.release(&format!("k{i}"), fake(), true);
        }
        assert_eq!(pool.idle_count(), MAX_IDLE_TOTAL);
        assert_eq!(pool.shutdown(), MAX_IDLE_TOTAL);
    }

    // Checks JSON conversions both ways.
    #[test]
    fn test_conversions() {
        let v = serde_json::json!({"a": [1, "x", null]});
        assert_eq!(from_sdk(&to_sdk(&v)), v);
        assert_eq!(module_path(Path::new("/w"), "ab"), PathBuf::from("/w/ab.cwasm"));
    }
}
