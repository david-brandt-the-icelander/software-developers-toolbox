// MCP proxy over standard streams: runs the server as a child and cleans its responses on the way back.
use super::{Filter, Pending};
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

// Copies client messages to the server line by line, recording the requests whose answers will be cleaned.
pub fn pump_requests(from: impl BufRead, mut to: impl Write, pending: &Mutex<Pending>) -> io::Result<()> {
    for line in from.split(b'\n') {
        let line = line?;
        if let Ok(text) = std::str::from_utf8(&line) {
            pending.lock().expect("pending lock").note_text(text);
        }
        to.write_all(&line)?;
        to.write_all(b"\n")?;
        to.flush()?;
    }
    Ok(())
}

// Copies server messages to the client line by line, cleaning the responses to recorded requests.
pub fn pump_responses(
    from: impl BufRead,
    mut to: impl Write,
    pending: &Mutex<Pending>,
    filter: &Filter,
) -> io::Result<()> {
    for line in from.split(b'\n') {
        let line = line?;
        let cleaned = std::str::from_utf8(&line)
            .ok()
            .and_then(|text| filter.rewrite_text(&mut pending.lock().expect("pending lock"), text));
        to.write_all(cleaned.as_ref().map_or(&line[..], |c| c.as_bytes()))?;
        to.write_all(b"\n")?;
        to.flush()?;
    }
    Ok(())
}

// Runs the server and relays until it closes its output; returns its exit code.
pub fn run_with(
    cmd: &[String],
    filter: &Filter,
    client_in: impl BufRead + Send + 'static,
    client_out: impl Write,
) -> Result<i32, String> {
    let (prog, args) = cmd.split_first().ok_or("no server command given")?;
    // No timeout: an MCP server lives as long as its client, and closing its stdin is how it is told to stop.
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot start {prog}: {e}"))?;
    let pending = Arc::new(Mutex::new(Pending::default()));
    let to_server = child.stdin.take().expect("stdin is piped");
    let from_server = child.stdout.take().expect("stdout is piped");
    let p = Arc::clone(&pending);
    std::thread::spawn(move || pump_requests(client_in, to_server, &p));
    let relayed = pump_responses(BufReader::new(from_server), client_out, &pending, filter);
    let status = child.wait().map_err(|e| format!("waiting for {prog}: {e}"))?;
    relayed.map_err(|e| format!("relaying {prog}'s output: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

// Runs the server between this process's standard input and output.
pub fn run(cmd: &[String], filter: &Filter) -> Result<i32, String> {
    run_with(cmd, filter, BufReader::new(io::stdin()), io::stdout().lock())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::io::Cursor;

    // A filter with the default settings.
    fn filter() -> Filter {
        Filter { lossy: true, budget: 8000, skip: Vec::new(), log: false, store: None }
    }

    // A writer that always fails.
    struct Broken;

    impl Write for Broken {
        // Always fails.
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("closed"))
        }

        // Never reached.
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // Checks requests are forwarded byte for byte and recorded, including non-UTF-8 lines.
    #[test]
    fn test_pump_requests() {
        let pending = Mutex::new(Pending::default());
        let input = b"{\"id\":1,\"method\":\"tools/call\"}\n\xff\xfe\nlast".to_vec();
        let mut out = Vec::new();
        pump_requests(Cursor::new(input), &mut out, &pending).expect("pump");
        assert_eq!(out, b"{\"id\":1,\"method\":\"tools/call\"}\n\xff\xfe\nlast\n");
        assert!(!pending.lock().expect("lock").is_empty());
        assert!(pump_requests(Cursor::new(b"x\n".to_vec()), Broken, &pending).is_err());
    }

    // Checks responses to recorded calls are cleaned and every other line passes through unchanged.
    #[test]
    fn test_pump_responses() {
        let pending = Mutex::new(Pending::default());
        pending.lock().expect("lock").note_text(r#"{"id":1,"method":"tools/call","params":{"name":"t"}}"#);
        let input =
            "{\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"{\\n  \\\"a\\\": 1\\n}\"}]}}\n\
{\"method\":\"notifications/progress\"}\n";
        let mut bytes = input.as_bytes().to_vec();
        bytes.extend_from_slice(b"\xff\n");
        let mut out = Vec::new();
        pump_responses(Cursor::new(bytes), &mut out, &pending, &filter()).expect("pump");
        let text = String::from_utf8_lossy(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains(r#""text":"{\"a\":1}""#), "{}", lines[0]);
        assert_eq!(lines[1], "{\"method\":\"notifications/progress\"}");
        assert!(pump_responses(Cursor::new(b"x\n".to_vec()), Broken, &pending, &filter()).is_err());
    }

    // Checks a real child process: requests reach it, its cleaned answer comes back, and its exit code is returned.
    #[test]
    fn test_run_with_child() {
        let script = r#"read line; case "$line" in *tools/call*) printf '%s\n' '{"id":7,"result":{"content":[{"type":"text","text":"{\n  \"b\": 2\n}"}]}}';; esac; exit 3"#;
        let cmd: Vec<String> = ["sh", "-c", script].iter().map(|s| s.to_string()).collect();
        let input =
            Cursor::new(b"{\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"x\"}}\n".to_vec());
        let mut out = Vec::new();
        let code = run_with(&cmd, &filter(), input, &mut out).expect("run");
        assert_eq!(code, 3);
        assert!(String::from_utf8_lossy(&out).contains(r#""text":"{\"b\":2}""#));
    }

    // Checks the errors for a missing command, a program that cannot start and a client that cannot be written.
    #[test]
    fn test_run_with_errors() {
        let empty = Cursor::new(Vec::new());
        assert_eq!(
            run_with(&[], &filter(), empty.clone(), Vec::new()).unwrap_err(),
            "no server command given"
        );
        let missing = vec!["/nonexistent/zazzle-server".to_string()];
        assert!(run_with(&missing, &filter(), empty.clone(), Vec::new())
            .unwrap_err()
            .starts_with("cannot start /nonexistent/zazzle-server: "));
        let talk: Vec<String> = ["sh", "-c", "echo hi"].iter().map(|s| s.to_string()).collect();
        assert!(run_with(&talk, &filter(), empty, Broken).unwrap_err().starts_with("relaying sh's output: "));
    }
}
