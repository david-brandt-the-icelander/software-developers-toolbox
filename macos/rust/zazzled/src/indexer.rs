// Bridge to zazzle-csharp-indexer: asks its daemon for a C# file's terse view, starting the daemon when needed.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const BINARY: &str = "zazzle-csharp-indexer";
const START_WAIT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(20);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const HELLO_TRIES: usize = 3;

// Where the indexer is: its executable and the state directory that holds its daemon socket.
#[derive(Debug, Clone, PartialEq)]
pub struct Indexer {
    pub exe: PathBuf,
    pub state: PathBuf,
}

// The indexer version per executable, asked once per process.
static VERSIONS: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

// Returns an environment variable when it is set and not empty.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

// Finds the indexer executable: $ZAZZLE_INDEXER, else beside this executable, else on PATH.
fn find_exe() -> Option<PathBuf> {
    if let Some(p) = env("ZAZZLE_INDEXER") {
        return Some(PathBuf::from(p));
    }
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(BINARY)));
    let on_path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join(BINARY))
        .collect::<Vec<_>>();
    beside.into_iter().chain(on_path).find(|p| p.is_file())
}

// Returns the indexer's state directory, as the indexer itself chooses it.
fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = env("ZAZZLE_INDEXER_STATE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = PathBuf::from(env("HOME")?);
    if cfg!(target_os = "macos") {
        return Some(home.join("Library/Application Support/zazzle-csharp-indexer"));
    }
    Some(
        env("XDG_STATE_HOME")
            .map_or_else(|| home.join(".local/state"), PathBuf::from)
            .join("zazzle-csharp-indexer"),
    )
}

// Returns the git work tree that contains a path.
pub fn work_tree(path: &Path) -> Option<&Path> {
    path.ancestors().skip(1).find(|a| a.join(".git").exists())
}

// Runs a command with a timeout and returns its standard output.
fn output_within(cmd: &mut Command, limit: Duration) -> Result<String, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let end = Instant::now() + limit;
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            return Err("timed out".into());
        }
        std::thread::sleep(POLL);
    }
    let mut out = String::new();
    child.stdout.take().expect("stdout is piped").read_to_string(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

// Sends one JSON line.
fn send(w: &mut UnixStream, v: &Value) -> Result<(), String> {
    w.write_all(format!("{v}\n").as_bytes()).map_err(|e| format!("indexer connection: {e}"))
}

// Reads one JSON line.
fn receive(r: &mut impl BufRead) -> Result<Value, String> {
    let mut line = String::new();
    if r.read_line(&mut line).map_err(|e| format!("indexer connection: {e}"))? == 0 {
        return Err("indexer closed the connection".into());
    }
    serde_json::from_str(&line).map_err(|_| "indexer sent something that is not JSON".into())
}

// Takes the text of a tools/call result, or the error it reports.
fn result_text(msg: &Value) -> Result<String, String> {
    if let Some(e) = msg.get("error") {
        return Err(format!("indexer error: {}", e["message"].as_str().unwrap_or("unknown")));
    }
    let text =
        msg.pointer("/result/content/0/text").and_then(Value::as_str).ok_or("indexer reply has no text")?;
    if msg.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
        return Err(text.to_string());
    }
    Ok(text.to_string())
}

impl Indexer {
    // Finds the indexer and its state directory, or says why there is none.
    pub fn locate() -> Result<Indexer, String> {
        let exe = find_exe().ok_or("zazzle-csharp-indexer is not installed (beside zazzled or on PATH)")?;
        let state = state_dir().ok_or("HOME is not set")?;
        Ok(Indexer { exe, state })
    }

    // Returns the indexer's version, which the daemon checks; asked once per executable.
    fn version(&self) -> Result<String, String> {
        let mut known = VERSIONS.lock().expect("version lock");
        if let Some((_, v)) = known.iter().find(|(p, _)| *p == self.exe) {
            return Ok(v.clone());
        }
        let out = output_within(Command::new(&self.exe).arg("--version"), VERSION_TIMEOUT)
            .map_err(|e| format!("{BINARY} --version: {e}"))?;
        let v = out
            .split_whitespace()
            .last()
            .ok_or_else(|| format!("{BINARY} --version printed nothing"))?
            .to_string();
        known.push((self.exe.clone(), v.clone()));
        Ok(v)
    }

    // Starts the indexer daemon detached; a thread reaps it when it exits.
    fn start_daemon(&self) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new(&self.exe)
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot start the indexer daemon: {e}"))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }

    // Connects to the daemon socket, starting the daemon and waiting for it when nothing listens.
    fn connect(&self) -> Result<UnixStream, String> {
        let sock = self.state.join("daemon.sock");
        if let Ok(s) = UnixStream::connect(&sock) {
            return Ok(s);
        }
        self.start_daemon()?;
        let end = Instant::now() + START_WAIT;
        loop {
            if let Ok(s) = UnixStream::connect(&sock) {
                return Ok(s);
            }
            if Instant::now() > end {
                return Err("the indexer daemon did not start".into());
            }
            std::thread::sleep(POLL * 5);
        }
    }

    // Asks the daemon to run one tool on a root, retrying while an older daemon restarts for a new version.
    fn call(&self, root: &Path, tool: &str, args: Value) -> Result<String, String> {
        let name =
            root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
        let hello = json!({"version": self.version()?, "roots": [format!("{name}={}", root.display())]});
        for _ in 0..HELLO_TRIES {
            let mut w = self.connect()?;
            let _ = w.set_read_timeout(Some(IO_TIMEOUT));
            let _ = w.set_write_timeout(Some(IO_TIMEOUT));
            let mut r = BufReader::new(w.try_clone().map_err(|e| format!("indexer connection: {e}"))?);
            send(&mut w, &hello)?;
            let reply = receive(&mut r)?;
            if reply["ok"] != json!(true) {
                if reply["restart"] == json!(true) {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                return Err(format!("indexer refused: {}", reply["error"].as_str().unwrap_or("no reason")));
            }
            let mut args = args.clone();
            args["path"] = json!(format!("{name}:{}", args["path"].as_str().unwrap_or_default()));
            send(
                &mut w,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}),
            )?;
            loop {
                let msg = receive(&mut r)?;
                if msg["id"] == json!(1) {
                    return result_text(&msg);
                }
            }
        }
        Err("the indexer daemon kept restarting".into())
    }

    // Returns the terse view of a C# file: a header line, then `N`, `N~` and `A-B` lines from the indexer.
    pub fn view(&self, path: &Path) -> Result<String, String> {
        let root = work_tree(path).ok_or("not inside a git work tree")?;
        let rel = path.strip_prefix(root).map_err(|_| "not inside its work tree")?;
        self.call(root, "view", json!({"path": rel.to_string_lossy()}))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    // A stand-in indexer: a script answering --version, and a daemon thread speaking the handshake and one tool call per connection.
    pub struct FakeIndexer {
        pub dir: TempDir,
        pub exe: PathBuf,
    }

    impl FakeIndexer {
        // Writes the version script and starts a daemon whose replies come from `answer`, given the hello and the request.
        pub fn start(answer: fn(&Value, &Value) -> (Value, Option<Value>)) -> FakeIndexer {
            let dir = TempDir::new();
            let exe =
                dir.write("bin/zazzle-csharp-indexer", b"#!/bin/sh\necho zazzle-csharp-indexer 9.9.9\n");
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            let listener = UnixListener::bind(dir.path().join("daemon.sock")).expect("bind");
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut w = stream.try_clone().expect("clone");
                    let mut r = BufReader::new(stream);
                    let Ok(hello) = receive(&mut r) else { continue };
                    let (reply, result) = answer(&hello, &Value::Null);
                    let _ = send(&mut w, &reply);
                    if result.is_none() {
                        continue;
                    }
                    let Ok(req) = receive(&mut r) else { continue };
                    let _ = send(&mut w, &json!({"jsonrpc": "2.0", "method": "notifications/message"}));
                    let (_, result) = answer(&hello, &req);
                    let _ = send(&mut w, &result.expect("result"));
                }
            });
            FakeIndexer { dir, exe }
        }

        // The indexer location pointing at this stand-in.
        pub fn indexer(&self) -> Indexer {
            Indexer { exe: self.exe.clone(), state: self.dir.path().to_path_buf() }
        }
    }

    // A daemon that accepts any hello and answers the view with the request's path echoed into a fixed view.
    pub fn viewing(_hello: &Value, req: &Value) -> (Value, Option<Value>) {
        let path = req.pointer("/params/arguments/path").and_then(Value::as_str).unwrap_or("").to_string();
        let text = format!("{path} 6 lines; terse: 1 usings, 1 access modifiers\n1-2\t⋯ 2 using(s)\n3\t\n4~\tclass C\n5\t{{\n6\t}}");
        (
            json!({"ok": true}),
            Some(json!({"jsonrpc": "2.0", "id": 1, "result": {"content": [{"type": "text", "text": text}]}})),
        )
    }

    // Writes a git work tree with one C# file and returns it and the file's path.
    fn repo() -> (TempDir, PathBuf) {
        let t = TempDir::new();
        std::fs::create_dir_all(t.path().join(".git")).expect("git dir");
        let f = t.write("src/C.cs", b"using A;\nusing B;\n\npublic class C\n{\n}\n");
        (t, f)
    }

    // Checks a view round trip: handshake with the version and root, the tool call with root:path, and notifications skipped.
    #[test]
    fn test_view_round_trip() {
        let fake = FakeIndexer::start(viewing);
        let (t, f) = repo();
        let text = fake.indexer().view(&f).expect("view");
        let name = t.path().file_name().expect("name").to_string_lossy().into_owned();
        assert!(text.starts_with(&format!("{name}:src/C.cs 6 lines; terse: ")), "{text}");
        assert!(text.ends_with("4~\tclass C\n5\t{\n6\t}"));
    }

    // Checks refusals, tool errors, JSON-RPC errors and a file outside any work tree.
    #[test]
    fn test_view_failures() {
        let refuse = FakeIndexer::start(|_, _| (json!({"ok": false, "error": "bad roots"}), None));
        let (_t, f) = repo();
        assert_eq!(refuse.indexer().view(&f).unwrap_err(), "indexer refused: bad roots");
        let restart = FakeIndexer::start(|_, _| (json!({"ok": false, "restart": true}), None));
        assert_eq!(restart.indexer().view(&f).unwrap_err(), "the indexer daemon kept restarting");
        let tool_err = FakeIndexer::start(|_, _| {
            (
                json!({"ok": true}),
                Some(
                    json!({"id": 1, "result": {"content": [{"type": "text", "text": "not a C# file"}], "isError": true}}),
                ),
            )
        });
        assert_eq!(tool_err.indexer().view(&f).unwrap_err(), "not a C# file");
        let rpc_err = FakeIndexer::start(|_, _| {
            (json!({"ok": true}), Some(json!({"id": 1, "error": {"message": "boom"}})))
        });
        assert_eq!(rpc_err.indexer().view(&f).unwrap_err(), "indexer error: boom");
        let empty = FakeIndexer::start(|_, _| (json!({"ok": true}), Some(json!({"id": 1, "result": {}}))));
        assert_eq!(empty.indexer().view(&f).unwrap_err(), "indexer reply has no text");
        let loose = TempDir::new();
        let outside = loose.write("x.cs", b"class X {}\n");
        assert_eq!(empty.indexer().view(&outside).unwrap_err(), "not inside a git work tree");
    }

    // Checks the daemon is started when nothing listens, and reported when it never comes up.
    #[test]
    fn test_view_starts_daemon_or_reports() {
        let dir = TempDir::new();
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in --version) echo zazzle-csharp-indexer 1.0;; daemon) touch '{}';; esac\n",
            dir.path().join("started").display()
        );
        let exe = dir.write("zazzle-csharp-indexer", script.as_bytes());
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let (_t, f) = repo();
        let ix = Indexer { exe, state: dir.path().join("state") };
        assert_eq!(ix.view(&f).unwrap_err(), "the indexer daemon did not start");
        assert!(dir.path().join("started").exists());
        let missing = Indexer { exe: dir.path().join("nope"), state: dir.path().to_path_buf() };
        assert!(missing.view(&f).unwrap_err().starts_with("zazzle-csharp-indexer --version: "));
    }

    // Checks the daemon closing early and sending non-JSON are reported.
    #[test]
    fn test_view_broken_daemon() {
        let dir = TempDir::new();
        let exe = dir.write("zazzle-csharp-indexer", b"#!/bin/sh\necho zazzle-csharp-indexer 2.0\n");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let listener = UnixListener::bind(dir.path().join("daemon.sock")).expect("bind");
        std::thread::spawn(move || {
            let mut first = true;
            for mut stream in listener.incoming().flatten() {
                let _ = receive(&mut BufReader::new(stream.try_clone().expect("clone")));
                let _ = stream.write_all(if first { b"" as &[u8] } else { b"not json\n" });
                first = false;
            }
        });
        let (_t, f) = repo();
        let ix = Indexer { exe, state: dir.path().to_path_buf() };
        assert_eq!(ix.view(&f).unwrap_err(), "indexer closed the connection");
        assert_eq!(ix.view(&f).unwrap_err(), "indexer sent something that is not JSON");
    }

    // Checks the executable and state directory come from the environment, and a missing indexer is explained.
    #[test]
    fn test_locate() {
        let _g = env_lock();
        let saved: Vec<_> = ["ZAZZLE_INDEXER", "ZAZZLE_INDEXER_STATE_DIR", "XDG_STATE_HOME", "PATH"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        std::env::set_var("ZAZZLE_INDEXER", "/opt/ix");
        std::env::set_var("ZAZZLE_INDEXER_STATE_DIR", "/tmp/ixstate");
        assert_eq!(Indexer::locate(), Ok(Indexer { exe: "/opt/ix".into(), state: "/tmp/ixstate".into() }));
        std::env::remove_var("ZAZZLE_INDEXER_STATE_DIR");
        std::env::set_var("XDG_STATE_HOME", "/x/state");
        let state = Indexer::locate().expect("locate").state;
        if !cfg!(target_os = "macos") {
            assert_eq!(state, PathBuf::from("/x/state/zazzle-csharp-indexer"));
        }
        std::env::remove_var("ZAZZLE_INDEXER");
        std::env::set_var("PATH", "/nonexistent-dir");
        assert_eq!(
            Indexer::locate().unwrap_err(),
            "zazzle-csharp-indexer is not installed (beside zazzled or on PATH)"
        );
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}
