// `zazzle daemon start|stop|status` and `zazzle tools`: the daemon's lifecycle and the versions of the installed tools.
use crate::client;
use crate::cmd::VERSION;
use crate::launchd;
use crate::proto::Request;
use crate::restart;
use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

// The tools `zazzle TOOL` runs from the tools' folder, as `zazzle tools` lists them; keep in step with TOOLS in the launcher.
pub const TOOLS: &[&str] = &[
    "daemon",
    "zazz-script-runner",
    "fix-network",
    "free-disk-space",
    "git-tools",
    "ai-agent-rules",
    "csharp-indexer",
    "cyberspace-generator",
];

// How long `zazzle tools` waits for one tool's `--version`.
const VERSION_WAIT: Duration = Duration::from_secs(5);

// What a running daemon reported about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
    pub version: String,
    pub pid: i32,
    pub uptime: Option<u64>,
}

// Where the daemon's socket, program and LaunchAgent are, so tests can point elsewhere.
pub struct Env<'a> {
    pub sock: &'a Path,
    pub exe: &'a Path,
    pub home: &'a Path,
    pub macos: bool,
}

// Parses a daemon's status reply, which starts "daemon VERSION pid N up Ss".
pub fn parse(text: &str) -> Option<Running> {
    let (version, pid) = restart::parse_status(text)?;
    let words: Vec<&str> = text.split_whitespace().collect();
    let up = words.iter().position(|w| *w == "up").and_then(|i| words.get(i + 1));
    let uptime = up.and_then(|w| w.strip_suffix('s')).and_then(|n| n.parse().ok());
    Some(Running { version, pid, uptime })
}

// The request that asks a daemon how it is.
fn status_request() -> Request {
    Request { cwd: "/".into(), session: String::new(), args: vec!["status".into()] }
}

// Asks the daemon on `sock` how it is, without starting one; None when none answers.
pub fn ask(sock: &Path) -> Option<Running> {
    client::call(sock, &status_request()).ok().and_then(|(_, text)| parse(&text))
}

// Returns the program an installed LaunchAgent starts, or None when no LaunchAgent is installed.
fn agent_program(home: &Path) -> Option<PathBuf> {
    launchd::plist_program(&std::fs::read_to_string(launchd::plist_path(home)).ok()?)
}

// Builds `zazzle daemon status`, as text or with `json` as JSON, and its exit code: 0 when a daemon runs, 3 when none does.
pub fn status(env: &Env, json: bool) -> (String, u8) {
    let found = ask(env.sock);
    let agent = env.macos.then(|| agent_program(env.home));
    let current = found.as_ref().is_none_or(|r| r.version == VERSION);
    let code = if found.is_some() { 0 } else { 3 };
    if json {
        let v = json!({
            "tool": "daemon",
            "version": VERSION,
            "ok": found.is_some() && current,
            "running": found.is_some(),
            "running_version": found.as_ref().map(|r| r.version.clone()),
            "pid": found.as_ref().map(|r| r.pid),
            "uptime_secs": found.as_ref().and_then(|r| r.uptime),
            "socket": env.sock.display().to_string(),
            "exe": env.exe.display().to_string(),
            "launch_agent": agent.as_ref().map(|a| json!({
                "installed": a.is_some(),
                "program": a.as_ref().map(|p| p.display().to_string()),
            })),
        });
        return (v.to_string(), code);
    }
    let sock = env.sock.display();
    let mut lines = vec![match &found {
        Some(r) => {
            let up = r.uptime.map(|s| format!(", up {s}s")).unwrap_or_default();
            format!("running: daemon {} (pid {}{up}) on {sock}", r.version, r.pid)
        }
        None => format!("not running on {sock}: zazzle daemon start, or any zazzle command, starts it"),
    }];
    lines.push(format!("installed: daemon {VERSION} at {}", env.exe.display()));
    if !current {
        lines.push("the running daemon is another version: run zazzle daemon restart".into());
    }
    if let Some(a) = agent {
        lines.push(match a {
            Some(p) => format!("LaunchAgent: installed, starts {}", p.display()),
            None => "LaunchAgent: not installed (zazzle daemon install starts the daemon at login)".into(),
        });
    }
    (lines.join("\n"), code)
}

// Runs `zazzle daemon start`: starts a daemon with `spawn` unless one answers on `sock`, and waits until it does.
pub fn start(sock: &Path, spawn: &dyn Fn() -> io::Result<()>) -> Result<String, String> {
    if let Some(r) = ask(sock) {
        return Ok(format!("daemon {} is already running (pid {})", r.version, r.pid));
    }
    let (_, text) = client::call_autostart(sock, &status_request(), spawn)
        .map_err(|e| format!("could not start: {e}"))?;
    let r = parse(&text).ok_or_else(|| format!("the daemon gave an unexpected status: {text}"))?;
    Ok(format!("started daemon {} (pid {})", r.version, r.pid))
}

// Runs `zazzle daemon stop`: stops the daemon that answered (`found`) with `stop`; with a LaunchAgent (`agent`), says it comes back on the next request.
pub fn stop(found: Option<Running>, agent: bool, stop: &dyn Fn(i32) -> bool) -> Result<String, String> {
    let Some(r) = found else { return Ok("no daemon is running".into()) };
    if r.pid <= 1 || r.pid == std::process::id() as i32 {
        return Ok(format!("daemon {} reported pid {}; left alone", r.version, r.pid));
    }
    if !stop(r.pid) {
        return Err(format!("daemon {} (pid {}) did not stop", r.version, r.pid));
    }
    let mut out = format!("stopped daemon {} (pid {})", r.version, r.pid);
    if agent {
        out.push_str(
            "\nthe LaunchAgent starts it again on the next request; zazzle daemon uninstall removes it",
        );
    }
    Ok(out)
}

// One row of `zazzle tools`: a tool, the version it reports, where it is, and what is wrong with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
    pub note: Option<String>,
}

// Returns the version a `--version` reply names: the last word of its first line.
fn version_of(reply: &str) -> Option<String> {
    reply.lines().next()?.split_whitespace().last().map(str::to_string)
}

// Describes one tool at `path` with the reply `ask` gets from its `--version`; cyberspace-generator keeps its own versions.
fn describe(name: &str, path: Option<PathBuf>, ask: &dyn Fn(&Path) -> Result<String, String>) -> Tool {
    let Some(p) = path.filter(|p| p.is_file()) else {
        let note = if name == "ai-agent" { "downloaded on first use (zazzle agent llm)" } else { "missing" };
        return Tool { name: name.into(), version: None, path: None, note: Some(note.into()) };
    };
    let (version, note) = match ask(&p) {
        Ok(reply) => {
            let v = version_of(&reply);
            let own = name == "cyberspace-generator";
            let note = match v.as_deref() {
                None => Some("no version from --version".to_string()),
                Some(v) if !own && v != VERSION => Some(format!("not this release ({VERSION})")),
                _ => None,
            };
            (v, note)
        }
        Err(e) => (None, Some(format!("--version failed: {e}"))),
    };
    Tool { name: name.into(), version, path: Some(p), note }
}

// Lists every tool in the tools' folder `dir`, and ai-agent where `jit` finds it, asking each for its version with `ask`.
pub fn tools(dir: &Path, jit: Option<PathBuf>, ask: &dyn Fn(&Path) -> Result<String, String>) -> Vec<Tool> {
    let mut rows: Vec<Tool> = TOOLS.iter().map(|n| describe(n, Some(dir.join(n)), ask)).collect();
    rows.push(describe("ai-agent", jit, ask));
    rows
}

// Renders `zazzle tools` as aligned text or with `json` as JSON; the exit code is 1 when a shipped tool is missing or differs.
pub fn render_tools(rows: &[Tool], dir: &Path, json: bool) -> (String, u8) {
    let ok = rows.iter().all(|t| t.note.is_none() || t.name == "ai-agent" && t.path.is_none());
    let code = if ok { 0 } else { 1 };
    if json {
        let list: Vec<Value> = rows
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "version": t.version,
                    "path": t.path.as_ref().map(|p| p.display().to_string()),
                    "note": t.note,
                })
            })
            .collect();
        let v = json!({"tool": "zazzle", "version": VERSION, "ok": ok, "folder": dir.display().to_string(), "tools": list});
        return (v.to_string(), code);
    }
    let width = rows.iter().map(|t| t.name.len()).max().unwrap_or(0);
    let mut out = format!("zazzle {VERSION}: tools in {}\n", dir.display());
    for t in rows {
        let version = t.version.as_deref().unwrap_or("-");
        let note = t.note.as_ref().map(|n| format!("  ({n})")).unwrap_or_default();
        out.push_str(&format!("  {:width$}  {version}{note}\n", t.name));
    }
    (out.trim_end().to_string(), code)
}

// Runs a tool's `--version`, giving up after VERSION_WAIT.
pub fn ask_version(path: &Path) -> Result<String, String> {
    crate::indexer::output_within(std::process::Command::new(path).arg("--version"), VERSION_WAIT)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::server::tests::Running as Daemon;
    use crate::testutil::TempDir;

    // Checks status replies give version, pid and uptime, and anything else is refused.
    #[test]
    fn test_parse() {
        let r = parse("daemon 1.2.3 pid 42 up 9s cache 0 hot").expect("parsed");
        assert_eq!(r, Running { version: "1.2.3".into(), pid: 42, uptime: Some(9) });
        assert_eq!(parse("daemon 1.2.3 pid 42").expect("parsed").uptime, None);
        assert_eq!(parse("daemon 1.2.3 pid 42 up x").expect("parsed").uptime, None);
        assert_eq!(parse("zazzle 1 pid 2"), None);
    }

    // Checks status with no daemon, with one running, in JSON, and the LaunchAgent line on macOS.
    #[test]
    fn test_status() {
        let t = TempDir::new();
        let none = t.path().join("none.sock");
        let exe = t.path().join("daemon");
        let env = Env { sock: &none, exe: &exe, home: t.path(), macos: true };
        let (text, code) = status(&env, false);
        assert_eq!(code, 3);
        assert!(text.starts_with("not running on ") && text.contains("LaunchAgent: not installed"), "{text}");
        let (j, _) = status(&env, true);
        let v: Value = serde_json::from_str(&j).expect("json");
        assert_eq!(
            (v["tool"].as_str(), v["running"].as_bool(), v["ok"].as_bool()),
            (Some("daemon"), Some(false), Some(false))
        );
        assert_eq!(v["launch_agent"]["installed"], json!(false));
        let plist = launchd::plist_path(t.path());
        std::fs::create_dir_all(plist.parent().expect("parent")).expect("dir");
        std::fs::write(&plist, launchd::plist(&exe, &none)).expect("plist");
        assert!(status(&env, false).0.contains("LaunchAgent: installed, starts "));
        let d = Daemon::start(Duration::from_secs(30));
        let env = Env { sock: &d.sock, exe: &exe, home: t.path(), macos: false };
        let (text, code) = status(&env, false);
        assert_eq!(code, 0);
        assert!(
            text.starts_with(&format!("running: daemon {VERSION} (pid ")) && !text.contains("LaunchAgent"),
            "{text}"
        );
        let v: Value = serde_json::from_str(&status(&env, true).0).expect("json");
        assert_eq!((v["ok"].as_bool(), v["launch_agent"].is_null()), (Some(true), true));
    }

    // Checks start reports a daemon that already runs, starts one through `spawn`, and reports a spawn that fails.
    #[test]
    fn test_start() {
        let d = Daemon::start(Duration::from_secs(30));
        let never = || -> io::Result<()> { panic!("must not spawn") };
        assert!(start(&d.sock, &never).expect("running").contains("is already running"));
        let t = TempDir::new();
        let sock = t.path().join("late.sock");
        let later = std::cell::RefCell::new(None);
        let spawn = || {
            let d = Daemon::start(Duration::from_secs(30));
            std::os::unix::fs::symlink(&d.sock, &sock)?;
            *later.borrow_mut() = Some(d);
            Ok(())
        };
        assert!(start(&sock, &spawn)
            .expect("started")
            .starts_with(&format!("started daemon {VERSION} (pid ")));
        let fail = || Err(io::Error::other("no exec"));
        let err = start(&t.path().join("none.sock"), &fail).unwrap_err();
        assert!(err.starts_with("could not start"), "{err}");
    }

    // Checks stop with no daemon, a daemon in this process (left alone), and the stopped and stuck outcomes.
    #[test]
    fn test_stop() {
        let (gone, stuck) = (|_: i32| true, |_: i32| false);
        assert_eq!(stop(None, false, &gone).expect("none"), "no daemon is running");
        let me = Running { version: VERSION.into(), pid: std::process::id() as i32, uptime: None };
        assert!(stop(Some(me), false, &gone).expect("self").ends_with("left alone"));
        let other = Running { version: "1.0.0".into(), pid: 4242, uptime: Some(1) };
        let out = stop(Some(other.clone()), true, &gone).expect("stopped");
        assert!(out.starts_with("stopped daemon 1.0.0 (pid 4242)\nthe LaunchAgent"), "{out}");
        assert_eq!(
            stop(Some(other.clone()), false, &gone).expect("stopped"),
            "stopped daemon 1.0.0 (pid 4242)"
        );
        assert_eq!(stop(Some(other), false, &stuck).unwrap_err(), "daemon 1.0.0 (pid 4242) did not stop");
    }

    // Checks the tool list: present and current, another release, a failing --version, missing, and ai-agent.
    #[test]
    fn test_tools() {
        let t = TempDir::new();
        for n in TOOLS {
            std::fs::write(t.path().join(n), b"").expect("tool");
        }
        std::fs::remove_file(t.path().join("fix-network")).expect("rm");
        let ask = |p: &Path| match p.file_name().and_then(|n| n.to_str()) {
            Some("git-tools") => Ok("git-tools 0.9.9\ngit version 2".to_string()),
            Some("csharp-indexer") => Err("timed out".to_string()),
            Some("cyberspace-generator") => Ok("cyberspace-generator 5.0.0".to_string()),
            Some("zazz-script-runner") => Ok(String::new()),
            Some(n) => Ok(format!("{n} {VERSION}")),
            None => Err("no name".into()),
        };
        let rows = tools(t.path(), None, &ask);
        let row = |n: &str| rows.iter().find(|r| r.name == n).expect("row").clone();
        assert_eq!(row("daemon").note, None);
        assert_eq!(row("git-tools").note, Some(format!("not this release ({VERSION})")));
        assert_eq!(row("fix-network").note.as_deref(), Some("missing"));
        assert!(row("csharp-indexer").note.expect("note").starts_with("--version failed"));
        assert_eq!(
            (row("cyberspace-generator").version.as_deref(), row("cyberspace-generator").note),
            (Some("5.0.0"), None)
        );
        assert!(row("ai-agent").note.expect("note").starts_with("downloaded on first use"));
        assert_eq!(row("zazz-script-runner").note.as_deref(), Some("no version from --version"));
        let (text, code) = render_tools(&rows, t.path(), false);
        assert_eq!(code, 1);
        assert!(
            text.starts_with(&format!("zazzle {VERSION}: tools in ")) && text.contains("  fix-network "),
            "{text}"
        );
        let v: Value = serde_json::from_str(&render_tools(&rows, t.path(), true).0).expect("json");
        assert_eq!((v["tool"].as_str(), v["ok"].as_bool()), (Some("zazzle"), Some(false)));
        assert_eq!(v["tools"].as_array().expect("tools").len(), TOOLS.len() + 1);
        let fine: Vec<Tool> = rows.into_iter().filter(|r| r.note.is_none() || r.name == "ai-agent").collect();
        assert_eq!(render_tools(&fine, t.path(), false).1, 0);
        let jit = tools(t.path(), Some(t.path().join("daemon")), &ask);
        assert_eq!(jit.last().map(|r| (r.name.as_str(), r.note.clone())), Some(("ai-agent", None)));
        assert_eq!(version_of(""), None);
        assert!(ask_version(Path::new("/nonexistent/tool")).is_err());
    }
}
