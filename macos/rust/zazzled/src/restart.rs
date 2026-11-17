// Replaces a running daemon with the installed zazzled: `zazzled restart`, and the check `zazzled mcp` makes at start.
use crate::client;
use crate::cmd::VERSION;
use crate::launchd;
use crate::proto::Request;
use std::path::Path;
use std::time::{Duration, Instant};

const STOP_WAIT: Duration = Duration::from_secs(5);

// What to restart: always the running daemon, or only one older than this binary.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Always,
    OnlyOlder,
}

// Returns the version and process id from a daemon's status reply, which starts "zazzled VERSION pid N".
pub fn parse_status(text: &str) -> Option<(String, i32)> {
    let mut words = text.split_whitespace();
    if words.next()? != "zazzled" {
        return None;
    }
    let version = words.next()?.to_string();
    if words.next()? != "pid" {
        return None;
    }
    Some((version, words.next()?.parse().ok()?))
}

// Asks the daemon on `sock` for its version and process id, without starting one; None when none answers.
pub fn running(sock: &Path) -> Option<(String, i32)> {
    let req = Request { cwd: "/".into(), session: String::new(), args: vec!["status".into()] };
    client::call(sock, &req).ok().and_then(|(_, text)| parse_status(&text))
}

// Tells whether release version `a` is older than `b`; a development build or anything else unparsable never is.
pub fn older(a: &str, b: &str) -> bool {
    let parse = |v: &str| v.split('.').map(|p| p.parse::<u64>().ok()).collect::<Option<Vec<_>>>();
    matches!((parse(a), parse(b)), (Some(x), Some(y)) if x.len() == 3 && y.len() == 3 && x < y)
}

// Tells whether process `pid` still exists.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    unsafe { libc::kill(pid, 0) == 0 }
}

// Stops process `pid` with SIGTERM, then SIGKILL if it is still there after STOP_WAIT; returns true once it is gone.
fn stop(pid: i32) -> bool {
    // SAFETY: sends a signal to one process id the daemon reported as its own.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let start = Instant::now();
    while alive(pid) && start.elapsed() < STOP_WAIT {
        std::thread::sleep(Duration::from_millis(50));
    }
    if alive(pid) {
        // SAFETY: as above, for the same process id.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        std::thread::sleep(Duration::from_millis(200));
    }
    !alive(pid)
}

// Rewrites the LaunchAgent to start `exe` when its plist names another program (with OnlyOlder, only one that no
// longer exists, so a development build never takes over the agent); returns what it did.
fn refresh_agent(
    exe: &Path,
    sock: &Path,
    home: &Path,
    uid: u32,
    mode: Mode,
    run: &dyn Fn(&[&str]) -> Result<(), String>,
) -> Option<String> {
    let text = std::fs::read_to_string(launchd::plist_path(home)).ok()?;
    let program = launchd::plist_program(&text)?;
    if program == exe || (mode == Mode::OnlyOlder && program.exists()) {
        return None;
    }
    Some(match launchd::install(exe, sock, home, uid, run) {
        Ok(_) => format!("the LaunchAgent now starts {} (it named {})", exe.display(), program.display()),
        Err(e) => format!("could not update the LaunchAgent: {e}"),
    })
}

// Stops the daemon answering on `sock` (with OnlyOlder, only one older than this binary) and points a LaunchAgent at
// `exe`, so the next request starts the installed zazzled; returns what it did.
pub fn restart(
    sock: &Path,
    exe: &Path,
    home: &Path,
    uid: u32,
    mode: Mode,
    run: &dyn Fn(&[&str]) -> Result<(), String>,
) -> Result<String, String> {
    let agent = Agent { exe, sock, home, uid, run, macos: cfg!(target_os = "macos") };
    replace(running(sock), VERSION, mode, &stop, &agent)
}

// Where the installed zazzled is and how to reach the LaunchAgent that should start it.
struct Agent<'a> {
    exe: &'a Path,
    sock: &'a Path,
    home: &'a Path,
    uid: u32,
    run: &'a dyn Fn(&[&str]) -> Result<(), String>,
    macos: bool,
}

// Decides what to do with the daemon that answered (`found`) given this binary's version `current`, stops it with
// `stop` when it should go, and refreshes the LaunchAgent on macOS; returns what it did.
fn replace(
    found: Option<(String, i32)>,
    current: &str,
    mode: Mode,
    stop: &dyn Fn(i32) -> bool,
    agent: &Agent,
) -> Result<String, String> {
    let mut notes = Vec::new();
    match found {
        None => notes.push(format!("no daemon was running; the next request starts zazzled {current}")),
        Some((v, _)) if mode == Mode::OnlyOlder && !older(&v, current) => {
            notes.push(format!("zazzled {v} is running; nothing to replace"))
        }
        Some((v, pid)) if pid <= 1 || pid == std::process::id() as i32 => {
            notes.push(format!("zazzled {v} reported pid {pid}; left alone"))
        }
        Some((v, pid)) if stop(pid) => {
            notes.push(format!("stopped zazzled {v} (pid {pid}); the next request starts zazzled {current}"))
        }
        Some((v, pid)) => return Err(format!("zazzled {v} (pid {pid}) did not stop")),
    }
    if agent.macos {
        notes.extend(refresh_agent(agent.exe, agent.sock, agent.home, agent.uid, mode, agent.run));
    }
    Ok(notes.join("\n"))
}

// Returns a note for `zazzled --version` when the daemon answering on `sock` is another version than this binary.
pub fn version_note(sock: &Path) -> String {
    mismatch_note(running(sock))
}

// Returns the mismatch note for the daemon that answered, or nothing when it is this version or none answered.
fn mismatch_note(found: Option<(String, i32)>) -> String {
    match found {
        Some((v, _)) if v != VERSION => {
            format!("\nthe running daemon is zazzled {v}; run `zazzled restart` so the installed one answers")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::server::tests::Running;
    use crate::testutil::TempDir;
    use std::cell::RefCell;

    // Checks status replies are read for their version and process id, and anything else is refused.
    #[test]
    fn test_parse_status() {
        assert_eq!(parse_status("zazzled 1.2.16 pid 4242 up 9s cache 0"), Some(("1.2.16".into(), 4242)));
        assert_eq!(parse_status("zazzle 1.2.16 pid 1"), None);
        assert_eq!(parse_status("zazzled 1.2.16 up 9s"), None);
        assert_eq!(parse_status("zazzled 1.2.16 pid x"), None);
        assert_eq!(parse_status(""), None);
    }

    // Checks release versions compare numerically and development builds are never older.
    #[test]
    fn test_older() {
        assert!(older("1.2.16", "1.2.24"));
        assert!(older("1.9.0", "1.10.0"));
        assert!(!older("1.2.24", "1.2.24"));
        assert!(!older("1.3.0", "1.2.24"));
        assert!(!older("0.0.0-dev", "1.2.24"));
        assert!(!older("1.2.16", "0.0.0-dev"));
        assert!(!older("1.2", "1.2.24"));
        assert!(!older("1.2.16", "1.2"));
    }

    // Checks a stopped process is reported gone, and a process that ignores SIGTERM is killed.
    #[test]
    fn test_stop() {
        let mut quiet = std::process::Command::new("sleep").arg("30").spawn().expect("sleep");
        let pid = quiet.id() as i32;
        let reaper = std::thread::spawn(move || quiet.wait());
        assert!(stop(pid));
        drop(reaper.join());
        let mut stubborn = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .spawn()
            .expect("sh");
        let pid = stubborn.id() as i32;
        let reaper = std::thread::spawn(move || stubborn.wait());
        std::thread::sleep(Duration::from_millis(200));
        assert!(stop(pid), "SIGKILL ends a process that ignores SIGTERM");
        drop(reaper.join());
    }

    // Checks restart with no daemon, a daemon of this version under OnlyOlder, and the daemon reporting this
    // process's own pid, which is left alone.
    #[test]
    fn test_restart_cases() {
        let t = TempDir::new();
        let sock = t.path().join("none.sock");
        let no_run = |_: &[&str]| Ok(());
        let exe = Path::new("/opt/homebrew/opt/zazzle/bin/zazzled");
        let r = restart(&sock, exe, t.path(), 501, Mode::Always, &no_run).expect("restart");
        assert!(r.starts_with("no daemon was running"), "{r}");
        assert_eq!(version_note(&sock), "");
        let d = Running::start(Duration::from_secs(30));
        assert_eq!(running(&d.sock).map(|(v, _)| v), Some(VERSION.to_string()));
        let r = restart(&d.sock, exe, t.path(), 501, Mode::OnlyOlder, &no_run).expect("restart");
        assert_eq!(r, format!("zazzled {VERSION} is running; nothing to replace"));
        let r = restart(&d.sock, exe, t.path(), 501, Mode::Always, &no_run).expect("restart");
        assert!(r.ends_with("left alone"), "the test daemon runs in this process: {r}");
        assert_eq!(version_note(&d.sock), "");
    }

    // Checks every decision about the daemon that answered: replaced when older or asked, left alone when it is
    // this process or not older under OnlyOlder, an error when it does not stop, and the LaunchAgent step on macOS.
    #[test]
    fn test_replace_decisions() {
        let t = TempDir::new();
        let (sock, exe) = (t.path().join("s"), t.path().join("opt/zazzled"));
        let no_run = |_: &[&str]| Ok(());
        let agent = |macos| Agent { exe: &exe, sock: &sock, home: t.path(), uid: 501, run: &no_run, macos };
        let (gone, stuck) = (|_: i32| true, |_: i32| false);
        let old = Some(("1.0.0".to_string(), 4242));
        let r = replace(old.clone(), "1.2.24", Mode::OnlyOlder, &gone, &agent(false)).expect("stopped");
        assert!(
            r.starts_with("stopped zazzled 1.0.0 (pid 4242); the next request starts zazzled 1.2.24"),
            "{r}"
        );
        let r = replace(old.clone(), VERSION, Mode::Always, &gone, &agent(false)).expect("stopped");
        assert!(r.starts_with("stopped zazzled 1.0.0 (pid 4242)"), "{r}");
        let err = replace(old.clone(), VERSION, Mode::Always, &stuck, &agent(false)).unwrap_err();
        assert_eq!(err, "zazzled 1.0.0 (pid 4242) did not stop");
        let newer = Some(("999.0.0".to_string(), 4242));
        let r = replace(newer.clone(), "1.2.24", Mode::OnlyOlder, &stuck, &agent(false)).expect("kept");
        assert_eq!(r, "zazzled 999.0.0 is running; nothing to replace");
        assert!(replace(newer, VERSION, Mode::Always, &gone, &agent(false))
            .expect("stopped")
            .starts_with("stopped"));
        let r =
            replace(Some(("1.0.0".into(), 1)), VERSION, Mode::Always, &gone, &agent(false)).expect("kept");
        assert!(r.ends_with("left alone"), "{r}");
        let plist = launchd::plist_path(t.path());
        std::fs::create_dir_all(plist.parent().expect("parent")).expect("dir");
        std::fs::write(&plist, launchd::plist(&t.path().join("gone/zazzled"), &sock)).expect("plist");
        let r = replace(None, VERSION, Mode::Always, &gone, &agent(true)).expect("refreshed");
        assert!(r.starts_with("no daemon was running") && r.contains("\nthe LaunchAgent now starts"), "{r}");
    }

    // Checks the version note appears only for a daemon of another version.
    #[test]
    fn test_mismatch_note() {
        assert_eq!(mismatch_note(None), "");
        assert_eq!(mismatch_note(Some((VERSION.into(), 7))), "");
        let note = mismatch_note(Some(("1.2.16".into(), 7)));
        assert!(note.contains("zazzled 1.2.16") && note.contains("zazzled restart"), "{note}");
    }

    // Checks the LaunchAgent is rewritten when it names another program, and under OnlyOlder only when that program
    // is gone.
    #[test]
    fn test_refresh_agent() {
        let t = TempDir::new();
        let sock = t.path().join("s");
        let exe = t.path().join("opt/zazzled");
        let calls = RefCell::new(Vec::new());
        let record = |a: &[&str]| {
            calls.borrow_mut().push(a.join(" "));
            Ok(())
        };
        assert_eq!(refresh_agent(&exe, &sock, t.path(), 501, Mode::Always, &record), None, "no plist");
        let plist = launchd::plist_path(t.path());
        std::fs::create_dir_all(plist.parent().expect("parent")).expect("dir");
        std::fs::write(&plist, "<plist><dict></dict></plist>").expect("plist");
        assert_eq!(
            refresh_agent(&exe, &sock, t.path(), 501, Mode::Always, &record),
            None,
            "names no program"
        );
        std::fs::write(&plist, launchd::plist(&exe, &sock)).expect("plist");
        assert_eq!(refresh_agent(&exe, &sock, t.path(), 501, Mode::Always, &record), None, "already current");
        let present = t.path().join("Cellar/zazzled");
        std::fs::create_dir_all(present.parent().expect("parent")).expect("dir");
        std::fs::write(&present, b"").expect("binary");
        std::fs::write(&plist, launchd::plist(&present, &sock)).expect("plist");
        assert_eq!(
            refresh_agent(&exe, &sock, t.path(), 501, Mode::OnlyOlder, &record),
            None,
            "it still exists"
        );
        let note = refresh_agent(&exe, &sock, t.path(), 501, Mode::Always, &record).expect("rewritten");
        assert!(note.starts_with("the LaunchAgent now starts"), "{note}");
        assert_eq!(
            launchd::plist_program(&std::fs::read_to_string(&plist).expect("read")),
            Some(exe.clone())
        );
        std::fs::write(&plist, launchd::plist(&t.path().join("gone/zazzled"), &sock)).expect("plist");
        assert!(refresh_agent(&exe, &sock, t.path(), 501, Mode::OnlyOlder, &record).is_some(), "dangling");
        let fail = |_: &[&str]| Err("denied".to_string());
        std::fs::write(&plist, launchd::plist(&present, &sock)).expect("plist");
        let note = refresh_agent(&exe, &sock, t.path(), 501, Mode::Always, &fail).expect("note");
        assert!(note.starts_with("could not update the LaunchAgent"), "{note}");
        assert!(calls.borrow().iter().any(|c| c.starts_with("bootstrap gui/501")));
    }
}
