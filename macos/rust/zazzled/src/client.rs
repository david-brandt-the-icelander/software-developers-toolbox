// Client side of the socket protocol, with daemon autostart for front ends that are not the C invoker.
use crate::proto::{self, Request};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const START_DEADLINE: Duration = Duration::from_secs(3);

// Returns the socket path from ZAZZLE_SOCK or the default under the home directory.
pub fn socket_path() -> io::Result<PathBuf> {
    crate::util::home_path(std::env::var_os("ZAZZLE_SOCK"), std::env::var_os("HOME"), ".zazzle/sock")
        .map_err(io::Error::other)
}

// Sends one request over a fresh connection and returns the status and text.
pub fn call(sock: &Path, req: &Request) -> io::Result<(u8, String)> {
    let mut s = UnixStream::connect(sock)?;
    proto::roundtrip(&mut s, req)
}

// Returns true if connecting failed because no daemon is listening.
fn not_running(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
}

// Calls the daemon, starting it with `spawn` and retrying if it is not running.
pub fn call_autostart(
    sock: &Path,
    req: &Request,
    spawn: &dyn Fn() -> io::Result<()>,
) -> io::Result<(u8, String)> {
    match call(sock, req) {
        Err(e) if not_running(&e) => {}
        other => return other,
    }
    spawn()?;
    let start = Instant::now();
    let mut wait = Duration::from_millis(2);
    loop {
        match call(sock, req) {
            Err(e) if not_running(&e) && start.elapsed() < START_DEADLINE => {
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

// Starts a detached daemon from this executable.
pub fn spawn_daemon() -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::server::tests::Running;
    use crate::testutil::TempDir;
    use std::cell::Cell;

    // Builds a version request.
    fn version_req() -> Request {
        Request { cwd: "/".into(), session: String::new(), args: vec!["version".into()] }
    }

    // Checks the socket path comes from the environment variables, in order.
    #[test]
    fn test_socket_path_resolution() {
        let _g = crate::testutil::env_lock();
        std::env::set_var("ZAZZLE_SOCK", "/tmp/custom.sock");
        assert_eq!(socket_path().expect("path"), PathBuf::from("/tmp/custom.sock"));
        std::env::remove_var("ZAZZLE_SOCK");
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set for tests"));
        assert_eq!(socket_path().expect("path"), home.join(".zazzle/sock"));
    }

    // Checks a plain call against a running server.
    #[test]
    fn test_call_reaches_server() {
        let srv = Running::start(Duration::from_secs(30));
        let (status, text) = call(&srv.sock, &version_req()).expect("call");
        assert_eq!(status, 0);
        assert!(text.starts_with("zazzle 0."));
        srv.finish().expect("finish");
    }

    // Checks autostart calls the spawner once and then succeeds.
    #[test]
    fn test_autostart_spawns_then_retries() {
        let dir = TempDir::new();
        let sock = dir.path().join("late.sock");
        let spawned = Cell::new(0);
        let started: std::cell::RefCell<Option<Running>> = std::cell::RefCell::new(None);
        let sock2 = sock.clone();
        let spawn = || {
            spawned.set(spawned.get() + 1);
            let l = crate::server::bind(&sock2)?;
            let engine = std::sync::Arc::new(crate::engine::Engine::new());
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            std::thread::spawn(move || {
                crate::server::serve(
                    l,
                    engine,
                    &crate::server::Config { idle: Duration::from_millis(300) },
                    &stop,
                )
            });
            Ok(())
        };
        let (status, _) = call_autostart(&sock, &version_req(), &spawn).expect("autostart");
        assert_eq!((status, spawned.get()), (0, 1));
        drop(started);
    }

    // Checks a spawner failure is returned.
    #[test]
    fn test_autostart_spawn_failure() {
        let dir = TempDir::new();
        let err = call_autostart(&dir.path().join("none"), &version_req(), &|| {
            Err(io::Error::other("no daemon binary"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "no daemon binary");
    }

    // Checks that giving up after the deadline returns the connection error.
    #[test]
    fn test_autostart_gives_up() {
        let dir = TempDir::new();
        let start = Instant::now();
        let err = call_autostart(&dir.path().join("never"), &version_req(), &|| Ok(())).unwrap_err();
        assert!(not_running(&err));
        assert!(start.elapsed() >= START_DEADLINE);
    }

    // Checks other connection errors are returned immediately without spawning.
    #[test]
    fn test_other_errors_do_not_spawn() {
        let dir = TempDir::new();
        let file = dir.write("not-a-dir", b"x");
        let err =
            call_autostart(&file.join("sock"), &version_req(), &|| panic!("must not spawn")).unwrap_err();
        assert!(!not_running(&err));
    }

    // Checks a non-start error that appears during the retry loop is returned as is.
    #[test]
    fn test_autostart_returns_other_error_after_spawn() {
        let dir = TempDir::new();
        let blocker = dir.path().join("f");
        let sock = blocker.join("sock");
        let b2 = blocker.clone();
        let spawn = move || std::fs::write(&b2, b"x");
        let err = call_autostart(&sock, &version_req(), &spawn).unwrap_err();
        assert!(!not_running(&err));
    }
}
