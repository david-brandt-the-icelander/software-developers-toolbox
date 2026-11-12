// Bounded subprocess execution, colors and the per-run log session.
use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value};

const SENSITIVE: &str = r"(?i)(pass(word)?|secret|token|psk|private.?key|shared.?secret)";

// Wrap text in an ANSI color code when writing to a terminal.
pub fn c(code: &str, text: &str) -> String {
    if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        return format!("\x1b[{code}m{text}\x1b[0m");
    }
    text.to_string()
}

// Return the root directory for per-run state.
fn state_root() -> PathBuf {
    if let Some(root) = std::env::var_os("FIXNET_STATE") {
        return PathBuf::from(root);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state/fix-network")
}

// Return the first capture group of a pattern in text.
pub fn capture(pattern: &str, text: &str) -> Option<String> {
    let re = Regex::new(pattern).expect("valid regex");
    re.captures(text).map(|m| m[1].to_string())
}

// Quote a string for a POSIX shell.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

// One run's log directory, saved original settings and output mode.
pub struct Session {
    pub dir: PathBuf,
    pub log_path: PathBuf,
    pub original: Map<String, Value>,
    pub quiet: bool,
    sensitive: Regex,
}

impl Session {
    // Create the timestamped private state directory.
    pub fn new(quiet: bool) -> std::io::Result<Self> {
        let dir = state_root().join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            log_path: dir.join("diagnostic.log"),
            dir,
            original: Map::new(),
            quiet,
            sensitive: Regex::new(SENSITIVE).expect("valid regex"),
        })
    }

    // Drop lines that look like they hold secrets.
    pub fn redact(&self, text: &str) -> String {
        text.lines().filter(|l| !self.sensitive.is_match(l)).collect::<Vec<_>>().join("\n")
    }

    // Append a line, minus sensitive-looking text, to the log and optionally print it.
    pub fn log_to(&self, msg: &str, show: bool) {
        let line = format!("[{}] {}", chrono::Local::now().format("%H:%M:%S"), msg);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            let _ = writeln!(f, "{}", self.redact(&line));
        }
        if show && !self.quiet {
            println!("{msg}");
        }
    }

    // Log and print a line.
    pub fn log(&self, msg: &str) {
        self.log_to(msg, true);
    }

    // Write the recorded original settings to original.json.
    pub fn save_original(&self) {
        let text = serde_json::to_string_pretty(&self.original).unwrap_or_default();
        let _ = fs::write(self.dir.join("original.json"), text);
    }
}

// Read a child pipe to a string in a background thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut out = String::new();
        if let Some(mut p) = pipe {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            out = String::from_utf8_lossy(&buf).into_owned();
        }
        out
    })
}

// Run a bounded command, optionally via sudo, and return (exit code, output).
pub fn sh_opts(cmd: &[&str], timeout: u64, sudo: bool) -> (i32, String) {
    let mut argv: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
    // SAFETY: geteuid has no preconditions.
    if sudo && unsafe { libc::geteuid() } != 0 {
        let mut prefix = vec!["sudo".to_string()];
        if !std::io::stdin().is_terminal() {
            prefix.push("-n".to_string());
        }
        argv.splice(0..0, prefix);
    }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdout(Stdio::piped()).stderr(Stdio::piped());
    if !sudo {
        command.stdin(Stdio::null());
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return (127, e.to_string()),
    };
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = out.join().unwrap_or_default() + &err.join().unwrap_or_default();
                return (status.code().unwrap_or(-1), text.trim().to_string());
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return (124, format!("(timed out after {timeout}s)"));
            }
        }
    }
}

// Run a command with a 15 second limit and return (exit code, output).
pub fn sh(cmd: &[&str]) -> (i32, String) {
    sh_opts(cmd, 15, false)
}

// Pause for a number of seconds.
pub fn sleep(secs: u64) {
    thread::sleep(Duration::from_secs(secs));
}

// Unit tests for the runner helpers.
#[cfg(test)]
mod tests {
    use super::*;

    // Check shell quoting of embedded single quotes.
    #[test]
    fn test_shell_quote_escapes() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }

    // Check that a timed-out command reports 124 and a missing one 127.
    #[test]
    fn test_sh_timeout() {
        assert_eq!(sh_opts(&["sleep", "5"], 1, false).0, 124);
        assert_eq!(sh(&["no-such-binary-xyz"]).0, 127);
    }
}
