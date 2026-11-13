// Bridge to the C# indexer: started on first use as a child process, then relayed MCP requests over its pipes.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, PoisonError};

// A running indexer with the pipes to talk to it; dropping it ends the child.
struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Drop for Running {
    // Closes the pipe so the indexer exits, then makes sure of it.
    fn drop(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Running {
    // Sends one JSON-RPC request and returns its result, or the error text; replies to other ids are skipped.
    fn ask(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line).map_err(|e| format!("indexer read failed: {e}"))? == 0 {
                return Err("indexer closed its output".into());
            }
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(e) = msg.get("error") {
                return Err(e["message"].as_str().unwrap_or("indexer error").to_string());
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    // Writes one JSON line to the indexer.
    fn send(&mut self, msg: &Value) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("indexer input closed")?;
        writeln!(stdin, "{msg}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("indexer write failed: {e}"))
    }
}

// Starts the indexer on one root and completes the MCP handshake.
fn start(program: &PathBuf, root: &str) -> Result<Running, String> {
    let mut child = Command::new(program)
        .args(["serve", "--root", root])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", program.display()))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().map(BufReader::new);
    let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
        let _ = child.kill();
        return Err("indexer pipes unavailable".into());
    };
    let mut running = Running { child, stdin: Some(stdin), stdout, next: 0 };
    let hello = json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "zazzled", "version": crate::cmd::VERSION}});
    running.ask("initialize", hello)?;
    running.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
    Ok(running)
}

// The link to the indexer: idle until the first request, then running or failed for good.
pub struct IndexerLink {
    program: PathBuf,
    root: String,
    state: Mutex<Option<Result<Running, String>>>,
}

impl IndexerLink {
    // Makes a link that will run program on root when first used.
    pub fn new(program: PathBuf, root: String) -> IndexerLink {
        IndexerLink { program, root, state: Mutex::new(None) }
    }

    // Chooses the program: $ZAZZLE_INDEXER, else zazzle-csharp-indexer beside this executable.
    pub fn default_program() -> PathBuf {
        if let Some(p) = std::env::var_os("ZAZZLE_INDEXER") {
            return PathBuf::from(p);
        }
        let exe = std::env::current_exe().unwrap_or_default();
        exe.with_file_name("zazzle-csharp-indexer")
    }

    // Sends a request to the indexer, starting it first if needed; a failed start is remembered.
    pub fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = guard.get_or_insert_with(|| start(&self.program, &self.root));
        let out = match slot {
            Ok(running) => running.ask(method, params),
            Err(e) => return Err(e.clone()),
        };
        if let Err(e) = &out {
            if e.starts_with("indexer closed")
                || e.starts_with("indexer write")
                || e.starts_with("indexer read")
            {
                *guard = Some(Err(e.clone()));
            }
        }
        out
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::PermissionsExt;

    // Writes an executable shell script standing in for the indexer and returns a link to it.
    fn link(dir: &TempDir, body: &str) -> IndexerLink {
        let path = dir.write("indexer", format!("#!/bin/sh\n{body}\n").as_bytes());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        IndexerLink::new(path, "/repo".into())
    }

    // Checks the handshake, noise and foreign ids being skipped, results, errors and the closed-pipe path.
    #[test]
    fn test_requests_and_failure() {
        let dir = TempDir::new();
        let l = link(
            &dir,
            r#"read a; echo '{"jsonrpc":"2.0","id":1,"result":{}}'; read b
read c; echo noise; echo '{"jsonrpc":"2.0","id":99,"result":{}}'; echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}'
read d; echo '{"jsonrpc":"2.0","id":3,"error":{"code":-1,"message":"boom"}}'
read e; echo '{"jsonrpc":"2.0","id":4,"error":{"code":-1}}'
read f"#,
        );
        assert_eq!(l.request("tools/list", json!({})), Ok(json!({"tools": []})));
        assert_eq!(l.request("tools/call", json!({})), Err("boom".into()));
        assert_eq!(l.request("tools/call", json!({})), Err("indexer error".into()));
        let closed = l.request("tools/call", json!({})).expect_err("closed");
        assert!(closed.starts_with("indexer "), "{closed}");
        assert_eq!(l.request("tools/list", json!({})), Err(closed));
    }

    // Checks a missing program, and an indexer that exits before the handshake, are remembered as failures.
    #[test]
    fn test_start_failures() {
        let missing = IndexerLink::new("/nonexistent/indexer".into(), "/".into());
        let e = missing.request("tools/list", json!({})).expect_err("missing");
        assert!(e.starts_with("cannot start /nonexistent/indexer"), "{e}");
        assert_eq!(missing.request("tools/list", json!({})), Err(e));
        let dir = TempDir::new();
        let l = link(&dir, "exit 0");
        let e = l.request("tools/list", json!({})).expect_err("exited");
        assert!(e.starts_with("indexer "), "{e}");
    }

    // Checks the default program sits beside this executable.
    #[test]
    fn test_default_program_name() {
        if std::env::var_os("ZAZZLE_INDEXER").is_none() {
            assert!(IndexerLink::default_program().ends_with("zazzle-csharp-indexer"));
        }
    }
}
