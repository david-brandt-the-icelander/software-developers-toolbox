// The activity log: one JSON line per zazzle command, run, proxied MCP call and hook decision, the evidence `zazzle setup --report` reads.
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// The log is renamed to `activity.log.1` once it passes this size, so it never grows without bound.
const MAX_BYTES: u64 = 2 << 20;

// What this process is, for proxy entries: the MCP server it fronts.
static LABEL: OnceLock<String> = OnceLock::new();

// Names the MCP server this proxy process fronts.
pub fn set_label(label: &str) {
    let _ = LABEL.set(label.to_string());
}

// Names the stdio MCP server this proxy process fronts by its program name and arguments.
pub fn set_command_label(cmd: &[String]) {
    let name = cmd.first().map(|c| c.rsplit('/').next().unwrap_or(c)).unwrap_or_default();
    let rest = cmd.iter().skip(1).map(String::as_str);
    set_label(
        &std::iter::once(name).chain(rest).collect::<Vec<_>>().join(" ").chars().take(60).collect::<String>(),
    );
}

// Returns the log file: $ZAZZLE_ACTIVITY_LOG, else ~/.zazzle/activity.log; tests log nowhere unless they set the variable.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ZAZZLE_ACTIVITY_LOG") {
        return (!p.is_empty()).then(|| PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zazzle/activity.log"))
}

// Returns the seconds since the epoch.
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// Appends one entry with the time and, for a proxy, its server; failures are ignored, since logging must never break a command.
pub fn record(mut entry: Value) {
    let Some(p) = path() else { return };
    entry["ts"] = json!(now());
    if let Some(l) = LABEL.get() {
        entry["server"] = json!(l);
        if entry.get("session").is_none() {
            entry["session"] = json!(std::env::var("ZAZZLE_SESSION").unwrap_or_default());
        }
    }
    let _ = append(&p, &entry);
}

// Writes one line, renaming a full log aside first.
fn append(p: &Path, entry: &Value) -> std::io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(p).is_ok_and(|m| m.len() > MAX_BYTES) {
        std::fs::rename(p, p.with_extension("log.1"))?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    writeln!(f, "{entry}")
}

// Reads the entries written at or after a time, older rotated entries first.
pub fn read(p: &Path, since: u64) -> Vec<Value> {
    [p.with_extension("log.1"), p.to_path_buf()]
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .flat_map(|t| t.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect::<Vec<_>>())
        .filter(|v| v["ts"].as_u64().unwrap_or(0) >= since)
        .collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};

    // Checks entries are appended with a time, read back by time, rotated past the size limit, and that tests log nowhere by default.
    #[test]
    fn test_record_read_rotate() {
        let _g = env_lock();
        let saved = std::env::var_os("ZAZZLE_ACTIVITY_LOG");
        std::env::remove_var("ZAZZLE_ACTIVITY_LOG");
        assert_eq!(path(), None);
        let t = TempDir::new();
        let p = t.path().join("sub/activity.log");
        std::env::set_var("ZAZZLE_ACTIVITY_LOG", &p);
        record(json!({"src": "zazzle", "cmd": "read"}));
        let got = read(&p, 0);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["cmd"], "read");
        assert!(read(&p, now() + 100).is_empty());
        std::fs::write(&p, vec![b'x'; (MAX_BYTES + 1) as usize]).expect("big");
        set_command_label(&["/usr/bin/srv".to_string(), "-x".to_string()]);
        record(json!({"src": "proxy"}));
        assert!(p.with_extension("log.1").exists());
        let got = read(&p, 0);
        assert_eq!((got.len(), got[0]["server"].as_str()), (1, Some("srv -x")));
        assert!(got[0]["session"].is_string());
        std::env::set_var("ZAZZLE_ACTIVITY_LOG", "");
        assert_eq!(path(), None);
        record(json!({}));
        std::env::set_var("ZAZZLE_ACTIVITY_LOG", t.path().join("f/activity.log"));
        std::fs::write(t.path().join("f"), b"file, not a folder").expect("write");
        record(json!({}));
        match saved {
            Some(v) => std::env::set_var("ZAZZLE_ACTIVITY_LOG", v),
            None => std::env::remove_var("ZAZZLE_ACTIVITY_LOG"),
        }
    }
}
