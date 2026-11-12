// The outside world behind one trait: commands, DNS, TCP, time, SSH detection and the Wi-Fi watchdog.
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::run::shell_quote;

// Everything fix-network does to or asks of the machine.
pub trait Sys {
    // Run a bounded command, optionally via sudo, and return (exit code, output).
    fn run(&self, cmd: &[&str], timeout: u64, sudo: bool) -> (i32, String);
    // Resolve a name to its addresses through the system resolver.
    fn resolve(&self, name: &str, timeout: u64) -> Result<Vec<IpAddr>, String>;
    // Tell whether a TCP connection to any of the addresses succeeds.
    fn tcp(&self, addrs: &[IpAddr], port: u16, timeout: u64) -> bool;
    // Wait a number of seconds.
    fn pause(&self, secs: u64);
    // Return the current Unix time in seconds.
    fn now(&self) -> u64;
    // Tell whether this process runs inside an SSH session.
    fn over_ssh(&self) -> bool;
    // Start a detached script that turns Wi-Fi back on after a delay.
    fn watchdog(&self, dir: &Path, dev: &str, delay: u64) -> io::Result<Child>;
}

// The real machine.
pub struct Real;

impl Sys for Real {
    // Run the command for real.
    fn run(&self, cmd: &[&str], timeout: u64, sudo: bool) -> (i32, String) {
        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        exec(&argv(cmd, sudo, euid, io::stdin().is_terminal()), timeout, sudo)
    }

    // Resolve with the system resolver.
    fn resolve(&self, name: &str, timeout: u64) -> Result<Vec<IpAddr>, String> {
        resolve_with(name, timeout, system_lookup)
    }

    // Connect for real.
    fn tcp(&self, addrs: &[IpAddr], port: u16, timeout: u64) -> bool {
        let limit = Duration::from_secs(timeout);
        addrs.iter().any(|ip| TcpStream::connect_timeout(&SocketAddr::new(*ip, port), limit).is_ok())
    }

    // Sleep for real.
    fn pause(&self, secs: u64) {
        thread::sleep(Duration::from_secs(secs));
    }

    // Read the system clock.
    fn now(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }

    // Read the SSH environment variables.
    fn over_ssh(&self) -> bool {
        ssh_from(std::env::var_os("SSH_CONNECTION"), std::env::var_os("SSH_TTY"))
    }

    // Write recover.sh and start it detached.
    fn watchdog(&self, dir: &Path, dev: &str, delay: u64) -> io::Result<Child> {
        let path = dir.join("recover.sh");
        fs::write(&path, recover_script(dir, dev, delay))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Command::new("/bin/sh")
            .arg(&path)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
}

// Return the watchdog script that forces Wi-Fi on after a delay and logs it.
pub fn recover_script(dir: &Path, dev: &str, delay: u64) -> String {
    let log = dir.join("diagnostic.log");
    format!(
        "#!/bin/sh\n# Re-enable Wi-Fi no matter what. Log: {}\nsleep {delay}\n\
         /usr/sbin/networksetup -setairportpower {} on\n\
         echo \"$(date) watchdog ensured Wi-Fi on\" >> {}\n",
        log.display(),
        shell_quote(dev),
        shell_quote(&log.to_string_lossy())
    )
}

// Build the argument list, prefixing sudo (non-interactive without a terminal) when not root.
pub fn argv(cmd: &[&str], sudo: bool, euid: u32, tty: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if sudo && euid != 0 {
        out.push("sudo".into());
        if !tty {
            out.push("-n".into());
        }
    }
    out.extend(cmd.iter().map(|s| s.to_string()));
    out
}

// Read a child pipe to a string in a background thread.
fn drain<R: Read + Send + 'static>(mut pipe: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

// Spawn an argument list with a time limit and return (exit code, stdout+stderr).
pub fn exec(argv: &[String], timeout: u64, interactive: bool) -> (i32, String) {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdout(Stdio::piped()).stderr(Stdio::piped());
    if !interactive {
        command.stdin(Stdio::null());
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return (127, e.to_string()),
    };
    let out = drain(child.stdout.take().expect("stdout is piped"));
    let err = drain(child.stderr.take().expect("stderr is piped"));
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = out.join().unwrap_or_default() + &err.join().unwrap_or_default();
                return (status.code().unwrap_or(-1), text.trim().to_string());
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return (124, format!("(timed out after {timeout}s)"));
            }
        }
    }
}

// Keep the first occurrence of each address, in order.
pub fn dedupe(addrs: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = Vec::new();
    for ip in addrs {
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    ips
}

// Look a name up with the system resolver.
fn system_lookup(host: String) -> Result<Vec<IpAddr>, String> {
    let addrs = (host.as_str(), 0).to_socket_addrs().map_err(|e| e.to_string())?;
    Ok(dedupe(addrs.map(|a| a.ip()).collect()))
}

// Tell whether SSH_CONNECTION or SSH_TTY marks an SSH session.
pub fn ssh_from(connection: Option<OsString>, tty: Option<OsString>) -> bool {
    connection.is_some() || tty.is_some()
}

// Resolve through a lookup function on a thread, with a time limit; IP literals need no lookup.
pub fn resolve_with(
    name: &str,
    timeout: u64,
    lookup: fn(String) -> Result<Vec<IpAddr>, String>,
) -> Result<Vec<IpAddr>, String> {
    if let Ok(ip) = name.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let (tx, rx) = mpsc::channel();
    let host = name.to_string();
    thread::spawn(move || {
        let _ = tx.send(lookup(host));
    });
    match rx.recv_timeout(Duration::from_secs(timeout)) {
        Ok(Ok(ips)) if ips.is_empty() => Err("no address".to_string()),
        Ok(result) => result,
        Err(_) => Err("timeout".to_string()),
    }
}

// A scripted machine for tests: canned command answers, names, open ports, clock and SSH flag.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod fake {
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::io;
    use std::net::IpAddr;
    use std::path::Path;
    use std::process::{Child, Command};

    use super::Sys;

    // Canned answers keyed by the command line; a queue answers in turn and repeats its last entry.
    #[derive(Default)]
    pub struct Fake {
        pub answers: RefCell<HashMap<String, VecDeque<(i32, String)>>>,
        pub calls: RefCell<Vec<String>>,
        pub names: RefCell<HashMap<String, Result<Vec<IpAddr>, String>>>,
        pub open: RefCell<Vec<(IpAddr, u16)>>,
        pub ssh: bool,
        pub clock: u64,
        pub watchdog_fails: bool,
        pub watchdog_done: bool,
    }

    impl Fake {
        // Set the answers for a command line, replacing earlier ones.
        pub fn on(&self, cmd: &str, answers: &[(i32, &str)]) -> &Self {
            let queue = answers.iter().map(|(rc, out)| (*rc, out.to_string())).collect();
            self.answers.borrow_mut().insert(cmd.to_string(), queue);
            self
        }

        // Make a name resolve to addresses.
        pub fn name(&self, host: &str, addrs: &[&str]) -> &Self {
            let ips = addrs.iter().map(|a| a.parse().expect("test address")).collect();
            self.names.borrow_mut().insert(host.to_string(), Ok(ips));
            self
        }

        // Accept TCP connections to an address and port.
        pub fn listen(&self, addr: &str, port: u16) -> &Self {
            self.open.borrow_mut().push((addr.parse().expect("test address"), port));
            self
        }

        // Return how many times a command line ran.
        pub fn count(&self, cmd: &str) -> usize {
            self.calls.borrow().iter().filter(|c| c.as_str() == cmd).count()
        }
    }

    impl Sys for Fake {
        // Answer from the script; unknown commands fail like a missing binary.
        fn run(&self, cmd: &[&str], _timeout: u64, sudo: bool) -> (i32, String) {
            let line = cmd.join(" ");
            self.calls.borrow_mut().push(if sudo { format!("sudo {line}") } else { line.clone() });
            let mut answers = self.answers.borrow_mut();
            let Some(queue) = answers.get_mut(&line) else {
                return (127, format!("unscripted: {line}"));
            };
            if queue.len() > 1 {
                return queue.pop_front().expect("non-empty queue");
            }
            queue.front().cloned().unwrap_or((0, String::new()))
        }

        // Answer from the name table; unknown names fail.
        fn resolve(&self, name: &str, _timeout: u64) -> Result<Vec<IpAddr>, String> {
            if let Ok(ip) = name.parse::<IpAddr>() {
                return Ok(vec![ip]);
            }
            self.names.borrow().get(name).cloned().unwrap_or(Err("nodename nor servname provided".into()))
        }

        // Succeed only for listening addresses.
        fn tcp(&self, addrs: &[IpAddr], port: u16, _timeout: u64) -> bool {
            addrs.iter().any(|a| self.open.borrow().contains(&(*a, port)))
        }

        // Do not wait.
        fn pause(&self, _secs: u64) {}

        // Return the fixed clock.
        fn now(&self) -> u64 {
            self.clock
        }

        // Return the fixed SSH flag.
        fn over_ssh(&self) -> bool {
            self.ssh
        }

        // Start a harmless process (already finished when told to), or fail when told to.
        fn watchdog(&self, _dir: &Path, dev: &str, _delay: u64) -> io::Result<Child> {
            self.calls.borrow_mut().push(format!("watchdog {dev}"));
            if self.watchdog_fails {
                return Err(io::Error::other("no space left"));
            }
            if self.watchdog_done {
                let mut child = Command::new("true").spawn()?;
                child.wait()?;
                return Ok(child);
            }
            Command::new("sleep").arg("5").spawn()
        }
    }
}

// Tests for the real machine implementation, run against localhost.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::net::TcpListener;

    use super::*;

    // Check sudo prefixing for root, non-root, terminal and no terminal.
    #[test]
    fn test_argv_sudo() {
        assert_eq!(argv(&["ls"], true, 0, false), vec!["ls"]);
        assert_eq!(argv(&["ls"], false, 501, false), vec!["ls"]);
        assert_eq!(argv(&["ls"], true, 501, true), vec!["sudo", "ls"]);
        assert_eq!(argv(&["ls"], true, 501, false), vec!["sudo", "-n", "ls"]);
    }

    // Check exit codes, combined output, timeouts and missing binaries.
    #[test]
    fn test_exec_outcomes() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            exec(&args(&["sh", "-c", "echo out; echo err >&2; exit 3"]), 5, false),
            (3, "out\nerr".into())
        );
        assert_eq!(exec(&args(&["sleep", "5"]), 1, true).0, 124);
        assert_eq!(exec(&args(&["no-such-binary-xyz"]), 5, false).0, 127);
        assert_eq!(Real.run(&["true"], 5, false), (0, String::new()));
    }

    // Check resolution of literals, localhost, empty answers, errors and timeouts.
    #[test]
    fn test_resolve_paths() {
        assert_eq!(Real.resolve("10.1.2.3", 1).unwrap(), vec!["10.1.2.3".parse::<IpAddr>().unwrap()]);
        assert!(Real.resolve("localhost", 5).unwrap().iter().all(|ip| ip.is_loopback()));
        assert_eq!(resolve_with("x", 2, |_| Ok(vec![])), Err("no address".into()));
        assert_eq!(resolve_with("x", 2, |_| Err("nxdomain".into())), Err("nxdomain".into()));
        let slow = |_: String| {
            thread::sleep(Duration::from_secs(3));
            Ok(vec![])
        };
        assert_eq!(resolve_with("x", 1, slow), Err("timeout".into()));
        assert_eq!(system_lookup("localhost".into()).map(|v| v.is_empty()), Ok(false));
    }

    // Check TCP success against a local listener and failure on a closed port.
    #[test]
    fn test_tcp_local() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(Real.tcp(&[lo], port, 2));
        drop(listener);
        assert!(!Real.tcp(&[lo], port, 2));
    }

    // Check the clock, pause, SSH flag and that the watchdog script runs and logs.
    #[test]
    fn test_real_misc_and_watchdog() {
        assert!(Real.now() > 1_700_000_000);
        Real.pause(0);
        assert_eq!(
            Real.over_ssh(),
            ssh_from(std::env::var_os("SSH_CONNECTION"), std::env::var_os("SSH_TTY"))
        );
        assert!(ssh_from(Some("1.2.3.4 22".into()), None) && ssh_from(None, Some("/dev/ttys001".into())));
        assert!(!ssh_from(None, None));
        let (a, b): (IpAddr, IpAddr) = ("10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap());
        assert_eq!(dedupe(vec![a, b, a]), vec![a, b]);
        let dir = std::env::temp_dir().join(format!("fixnet-wd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut child = Real.watchdog(&dir, "en0", 0).unwrap();
        assert!(child.wait().unwrap().success());
        let log = fs::read_to_string(dir.join("diagnostic.log")).unwrap();
        assert!(log.contains("watchdog ensured Wi-Fi on"));
        assert!(recover_script(&dir, "e'n", 45).contains(r"'e'\''n'"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
