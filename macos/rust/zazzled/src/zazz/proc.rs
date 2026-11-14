// Runs a subprocess with an argument list, optional stdin, captured output and a hard timeout.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// What a finished (or killed) subprocess produced.
#[derive(Debug)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl Ran {
    // True when the process exited with status 0 in time.
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    // Standard error as text, trimmed.
    pub fn err_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

// Reads a pipe to the end on a thread.
fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

// Runs a command to completion or until the timeout, then kills it.
pub fn run(cmd: &mut Command, input: Option<Vec<u8>>, timeout: Duration) -> Result<Ran, String> {
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))?;
    if let (Some(data), Some(mut sin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || drop(sin.write_all(&data)));
    }
    let out = drain(child.stdout.take().expect("stdout is piped"));
    let err = drain(child.stderr.take().expect("stderr is piped"));
    let start = Instant::now();
    let (code, timed_out) = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break (st.code(), false);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok(Ran { code, stdout, stderr, timed_out })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks output capture, stdin and exit codes.
    #[test]
    fn test_run_captures() {
        let r = run(
            Command::new("sh").args(["-c", "cat; echo err >&2; exit 3"]),
            Some(b"in".to_vec()),
            Duration::from_secs(5),
        )
        .expect("run");
        assert_eq!(
            (r.code, r.stdout.as_slice(), r.err_text().as_str(), r.ok()),
            (Some(3), &b"in"[..], "err", false)
        );
        assert!(run(Command::new("true").arg("x"), None, Duration::from_secs(5)).expect("true").ok());
    }

    // Checks the timeout kills the process, and a missing program is an error.
    #[test]
    fn test_run_timeout_and_missing() {
        let r = run(Command::new("sleep").arg("5"), None, Duration::from_millis(50)).expect("run");
        assert!(r.timed_out && !r.ok());
        assert!(run(&mut Command::new("/nonexistent/zazz"), None, Duration::from_secs(1)).is_err());
    }
}
