// End-to-end tests that run the afs binary: argument and stdin input, exit codes, the JSONL loop and the MCP server.
use std::io::Write;
use std::process::{Command, Output, Stdio};

// Run afs with arguments and optional stdin bytes, returning its output.
fn afs(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_afs"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afs starts");
    child.stdin.take().expect("stdin is piped").write_all(stdin).expect("stdin accepts input");
    child.wait_with_output().expect("afs finishes")
}

// Parse the stdout of a run as JSON.
fn json_out(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

// Parse every stdout line of a run as JSON.
fn json_lines(out: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("line is JSON"))
        .collect()
}

// Create a temp file and return its guard and path text.
fn sample() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "alpha\nbeta\n").unwrap();
    (dir, path.to_string_lossy().into_owned())
}

// A request in the argument succeeds with exit code 0.
#[test]
fn test_call_argument_success() {
    let (_d, path) = sample();
    let out = afs(&["call", &format!(r#"{{"op":"read","path":"{path}"}}"#)], b"");
    assert!(out.status.success());
    assert_eq!(json_out(&out)["total_lines"], 2);
}

// A request on stdin works both without an argument and with "-".
#[test]
fn test_call_stdin() {
    let (_d, path) = sample();
    let req = format!(r#"{{"op":"stat","path":"{path}"}}"#);
    for args in [&["call"][..], &["call", "-"][..]] {
        let out = afs(args, req.as_bytes());
        assert!(out.status.success());
        assert_eq!(json_out(&out)["kind"], "file");
    }
}

// Failed requests, invalid JSON and batches map to exit codes and replies.
#[test]
fn test_call_failures_and_batches() {
    let bad_op = afs(&["call", r#"{"op":"nope"}"#], b"");
    assert_eq!(bad_op.status.code(), Some(1));
    assert_eq!(json_out(&bad_op)["ok"], false);
    let invalid = afs(&["call", "{not json"], b"");
    assert_eq!(invalid.status.code(), Some(1));
    assert!(json_out(&invalid)["error"].as_str().unwrap().contains("invalid JSON"));
    let batch = afs(&["call", r#"[{"op":"stat","path":"/"},{"op":"stat","path":"/no/such"}]"#], b"");
    assert!(batch.status.success());
    assert_eq!(json_out(&batch).as_array().unwrap().len(), 2);
    let mixed = afs(&["call", r#"[{"op":"stat","path":"/"},{"op":"read","path":"/no/such"}]"#], b"");
    assert_eq!(mixed.status.code(), Some(1));
}

// Input that is not valid UTF-8 is reported on stderr with a failing exit code.
#[test]
fn test_call_unreadable_stdin() {
    let out = afs(&["call"], &[0xff, 0xfe]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("afs:"));
}

// The JSONL loop keeps one session, skips blank lines and answers bad lines with an error.
#[test]
fn test_jsonl_session() {
    let (_d, path) = sample();
    let req = format!(r#"{{"op":"read","path":"{path}"}}"#);
    let input = format!("{req}\n\n{req}\nnot json\n");
    let out = afs(&["jsonl"], input.as_bytes());
    let replies = json_lines(&out);
    assert_eq!(replies.len(), 3);
    assert!(replies[0].get("chunks").is_some());
    assert_eq!(replies[1]["unchanged"], true);
    assert_eq!(replies[2]["ok"], false);
}

// The JSONL loop stops with an error when its input is not valid UTF-8.
#[test]
fn test_jsonl_bad_input() {
    let out = afs(&["jsonl"], &[0xff, b'\n']);
    assert_eq!(out.status.code(), Some(1));
}

// The MCP server answers the handshake, lists tools, runs a tool and reports a parse error.
#[test]
fn test_serve_mcp() {
    let (_d, path) = sample();
    let call = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"afs_read","arguments":{{"path":"{path}"}}}}}}"#
    );
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        &call,
        "garbage",
    ]
    .join("\n");
    let replies = json_lines(&afs(&["serve"], input.as_bytes()));
    assert_eq!(replies.len(), 4);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 6);
    assert_eq!(replies[2]["result"]["isError"], false);
    assert_eq!(replies[3]["error"]["code"], -32700);
}

// The MCP server stops with an error when its input is not valid UTF-8.
#[test]
fn test_serve_bad_input() {
    assert_eq!(afs(&["serve"], &[0xff, b'\n']).status.code(), Some(1));
}

// Help and version print and succeed.
#[test]
fn test_help_and_version() {
    let help = afs(&["--help"], b"");
    assert!(help.status.success() && String::from_utf8_lossy(&help.stdout).contains("examples:"));
    assert!(afs(&["--version"], b"").status.success());
}
