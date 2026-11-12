// Colors, small text helpers and the per-run log session.
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Map, Value};

// A line that assigns a secret-looking key, such as "password: x", "psk = x" or "token=x".
const SENSITIVE: &str = r"(?i)(pass(word|wd|phrase)?|secret|token|psk|private.?key)\s*[:=]";

// Wrap text in an ANSI color code when enabled.
pub fn paint(code: &str, text: &str, enabled: bool) -> String {
    if enabled {
        return format!("\x1b[{code}m{text}\x1b[0m");
    }
    text.to_string()
}

// Tell whether to color: only on a terminal and without NO_COLOR.
pub fn color_enabled(tty: bool, no_color: Option<OsString>) -> bool {
    tty && no_color.is_none()
}

// Wrap text in an ANSI color code when writing to a terminal without NO_COLOR.
pub fn c(code: &str, text: &str) -> String {
    paint(code, text, color_enabled(std::io::stdout().is_terminal(), std::env::var_os("NO_COLOR")))
}

// Return the state root from FIXNET_STATE, else ~/.local/state/fix-network.
pub fn state_root_from(fixnet: Option<OsString>, home: Option<OsString>) -> PathBuf {
    match fixnet {
        Some(root) => PathBuf::from(root),
        None => PathBuf::from(home.unwrap_or_default()).join(".local/state/fix-network"),
    }
}

// Return the state root from the environment.
pub fn state_root() -> PathBuf {
    state_root_from(std::env::var_os("FIXNET_STATE"), std::env::var_os("HOME"))
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
    // Create a fresh private, timestamped state directory under the root.
    pub fn at(root: &Path, quiet: bool) -> std::io::Result<Self> {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let dir = root.join(stamp);
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

// Unit tests for the helpers and the session.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod tests {
    use super::*;

    // Create a quiet or printing session in a fresh temporary root of its own.
    pub fn session(quiet: bool) -> Session {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("fixnet-test-{}-{n}", std::process::id()));
        Session::at(&root, quiet).expect("temp session")
    }

    // Return everything a session logged.
    pub fn logged(s: &Session) -> String {
        fs::read_to_string(&s.log_path).unwrap_or_default()
    }

    // Check shell quoting of embedded single quotes.
    #[test]
    fn test_shell_quote_escapes() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }

    // Check coloring on and off.
    #[test]
    fn test_paint() {
        assert_eq!(paint("31", "x", true), "\x1b[31mx\x1b[0m");
        assert_eq!(paint("31", "x", false), "x");
        assert_eq!(c("31", "x"), "x");
        assert!(color_enabled(true, None));
        assert!(!color_enabled(true, Some("1".into())) && !color_enabled(false, None));
    }

    // Check the state root from FIXNET_STATE and from HOME.
    #[test]
    fn test_state_root_from() {
        assert_eq!(state_root_from(Some("/s".into()), Some("/h".into())), PathBuf::from("/s"));
        assert_eq!(state_root_from(None, Some("/h".into())), PathBuf::from("/h/.local/state/fix-network"));
        let _ = state_root();
    }

    // Check that logs drop secret-looking lines, quiet mode prints nothing, and originals are saved.
    #[test]
    fn test_session_log_redact_save() {
        let mut s = session(true);
        s.log("visible line");
        s.log("psk = hunter2");
        s.log_to("file only", false);
        s.log("Password: hunter3");
        s.log("token=abc123");
        s.log("sudo: a password is required; wiki bypasses tunnel; pass --internal-host");
        let text = logged(&s);
        assert!(text.contains("visible line") && text.contains("file only"));
        assert!(!text.contains("hunter2") && !text.contains("hunter3") && !text.contains("abc123"));
        assert!(text.contains("a password is required") && text.contains("bypasses tunnel"));
        s.original.insert("k".into(), Value::String("v".into()));
        s.save_original();
        assert!(fs::read_to_string(s.dir.join("original.json")).unwrap().contains("\"k\""));
        assert_eq!(fs::metadata(&s.dir).unwrap().permissions().mode() & 0o777, 0o700);
        session(false).log("printed to stdout");
        assert!(Session::at(Path::new("/proc/no-such/dir"), true).is_err());
        fs::remove_dir_all(&s.dir).unwrap();
        s.log("written nowhere: the directory is gone");
        assert!(!s.log_path.exists());
    }
}
