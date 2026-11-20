// Persistent privacy-preserving metrics for the command text actually returned to an agent.
use crate::tokens;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const ROTATE_BYTES: u64 = 20 * 1024 * 1024;

// A disabled log or one append-only JSONL file guarded across daemon workers.
#[derive(Default)]
pub struct DeliveryLog {
    path: Option<PathBuf>,
    lock: Mutex<()>,
}

impl DeliveryLog {
    // Opens $ZAZZLE_USAGE_LOG, or ~/.zazzle/usage.jsonl, and disables logging when neither path is available.
    pub fn open() -> DeliveryLog {
        let path = match std::env::var_os("ZAZZLE_USAGE_LOG") {
            Some(p) if p.is_empty() => None,
            Some(p) => Some(PathBuf::from(p)),
            None => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zazzle/usage.jsonl")),
        };
        DeliveryLog::at(path)
    }

    // Builds a log at an explicit path, used by the daemon and tests.
    pub fn at(path: Option<PathBuf>) -> DeliveryLog {
        DeliveryLog { path, lock: Mutex::new(()) }
    }

    // Appends final response sizes and hashes without retaining arguments or response text.
    pub fn record(
        &self,
        session: &str,
        command: &str,
        pre_dedupe_bytes: usize,
        response: &str,
        status: u8,
        deduplicated: bool,
    ) {
        let Some(path) = &self.path else { return };
        let _guard = self.lock.lock().expect("delivery log lock");
        if append(path, session, command, pre_dedupe_bytes, response, status, deduplicated).is_err() {}
    }
}

// Appends one record after creating and rotating the private log.
fn append(
    path: &Path,
    session: &str,
    command: &str,
    pre_dedupe_bytes: usize,
    response: &str,
    status: u8,
    deduplicated: bool,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(parent)?;
    }
    if path.metadata().is_ok_and(|m| m.len() > ROTATE_BYTES) {
        let _ = fs::rename(path, path.with_extension("jsonl.1"));
    }
    let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let delivered_bytes = response.len();
    let full_hash = blake3::hash(response.as_bytes()).to_hex().to_string();
    let hash = &full_hash[..12];
    let record = json!({
        "v": 1,
        "ts_ms": ts_ms,
        "session": session,
        "command": command,
        "pre_dedupe_bytes": pre_dedupe_bytes,
        "delivered_bytes": delivered_bytes,
        "pre_dedupe_est_tokens": tokens::from_bytes(pre_dedupe_bytes),
        "delivered_est_tokens": tokens::from_bytes(delivered_bytes),
        "response_hash": hash,
        "deduplicated": deduplicated,
        "status": status,
    });
    let mut file = OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    writeln!(file, "{record}")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};

    // Checks where the log goes: ZAZZLE_USAGE_LOG, nowhere when it is empty, else under HOME; and that a full log
    // rotates and a write that cannot happen is dropped quietly.
    #[test]
    fn test_open_rotate_and_unwritable() {
        let _g = env_lock();
        let t = TempDir::new();
        let (saved_log, saved_home) = (std::env::var_os("ZAZZLE_USAGE_LOG"), std::env::var_os("HOME"));
        std::env::set_var("ZAZZLE_USAGE_LOG", "");
        assert_eq!(DeliveryLog::open().path, None);
        DeliveryLog::open().record("s", "read", 1, "x", 0, false);
        std::env::set_var("ZAZZLE_USAGE_LOG", t.path().join("u.jsonl"));
        assert_eq!(DeliveryLog::open().path, Some(t.path().join("u.jsonl")));
        std::env::remove_var("ZAZZLE_USAGE_LOG");
        std::env::set_var("HOME", t.path());
        assert_eq!(DeliveryLog::open().path, Some(t.path().join(".zazzle/usage.jsonl")));
        match saved_log {
            Some(v) => std::env::set_var("ZAZZLE_USAGE_LOG", v),
            None => std::env::remove_var("ZAZZLE_USAGE_LOG"),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let full = t.path().join("full.jsonl");
        fs::File::create(&full).expect("create").set_len(ROTATE_BYTES + 1).expect("grow");
        DeliveryLog::at(Some(full.clone())).record("s", "read", 1, "x", 0, false);
        assert!(full.with_extension("jsonl.1").exists(), "the full log was rotated");
        assert!(fs::metadata(&full).expect("new log").len() < 1024);
        let blocked = t.write("file", b"x").join("usage.jsonl");
        DeliveryLog::at(Some(blocked.clone())).record("s", "read", 1, "x", 0, false);
        assert!(!blocked.exists());
    }

    // Checks records keep useful dimensions and hashes while omitting response text.
    #[test]
    fn test_record_is_private_and_text_free() {
        let t = TempDir::new();
        let path = t.path().join("usage.jsonl");
        DeliveryLog::at(Some(path.clone())).record("session-a", "read", 100, "secret response", 0, true);
        let text = fs::read_to_string(&path).expect("record");
        let record: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(record["session"], "session-a");
        assert_eq!(record["command"], "read");
        assert_eq!(record["pre_dedupe_bytes"], 100);
        assert_eq!(record["delivered_bytes"], 15);
        assert_eq!(record["deduplicated"], true);
        assert_eq!(record["response_hash"].as_str().expect("hash").len(), 12);
        assert!(!text.contains("secret response"));
    }
}
