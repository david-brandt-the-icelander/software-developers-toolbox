// The orchestration log: one privacy-preserving JSON line per adaptive run, with the plan, each part's attempts, every call's tokens and time, and the orchestrator's usage apart from the executors', so runs can be analysed and tuned.
use super::calls::{self, Call};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// A log this big is renamed to `.jsonl.1`, replacing the one before.
const ROTATE_BYTES: u64 = 20 * 1024 * 1024;

// What a run knows about itself that is not in its calls: the question's shape, the ladder, the orchestrator, how the plan and the combination went, each part's attempts and the final result.
pub struct Facts {
    pub question: String,
    pub scope: usize,
    pub symbols: usize,
    pub difficulty: usize,
    pub reasons: Vec<String>,
    pub ladder: Vec<String>,
    pub orchestrator: Option<String>,
    pub planner: Option<String>,
    pub plan: &'static str,
    pub parts: Vec<Value>,
    pub combine: &'static str,
    pub result: Value,
}

// Where the log goes: `$ZAZZLE_ORCHESTRATION_LOG`, else `~/.zazzle/orchestration.jsonl`; none when the variable is empty or there is no home (and none by default in this crate's own tests, which name a file when they want one).
pub fn path() -> Option<PathBuf> {
    locate(std::env::var_os("ZAZZLE_ORCHESTRATION_LOG"), std::env::var_os("HOME").filter(|_| !cfg!(test)))
}

// Picks the log's path from the variable's value and the home folder.
pub fn locate(var: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    match var {
        Some(p) if p.is_empty() => None,
        Some(p) => Some(PathBuf::from(p)),
        None => home.map(|h| PathBuf::from(h).join(".zazzle/orchestration.jsonl")),
    }
}

// Builds the record: no question text, only its hash and size, and every call's role, agent, sizes, tokens and seconds.
pub fn build(f: &Facts, calls: &[Call], secs: f64) -> Value {
    let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let hash = blake3::hash(f.question.as_bytes()).to_hex().to_string();
    json!({
        "v": 1,
        "kind": "adaptive",
        "ts_ms": ts_ms,
        "question_hash": &hash[..12],
        "question_words": f.question.split_whitespace().count(),
        "scope_paths": f.scope,
        "symbols": f.symbols,
        "difficulty": f.difficulty,
        "reasons": f.reasons,
        "ladder": f.ladder,
        "orchestrator": f.orchestrator,
        "planner": f.planner,
        "plan": f.plan,
        "parts": f.parts,
        "combine": f.combine,
        "result": f.result,
        "seconds": (secs * 10.0).round() / 10.0,
        "usage": calls::usage(calls, secs),
        "calls": calls.iter().map(call).collect::<Vec<_>>(),
    })
}

// The record of one `zazzle flow` or `zazzle review` command: its name, seconds, every call and the usage with the orchestrator apart from the execution agents; the caller adds the orchestrator's label.
pub fn flow(command: &str, calls: &[Call], secs: f64) -> Value {
    let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    json!({
        "v": 1,
        "kind": "flow",
        "ts_ms": ts_ms,
        "command": command,
        "seconds": (secs * 10.0).round() / 10.0,
        "usage": calls::usage(calls, secs),
        "calls": calls.iter().map(call).collect::<Vec<_>>(),
    })
}

// One call as the log keeps it.
fn call(c: &Call) -> Value {
    json!({
        "phase": c.phase(),
        "role": c.role,
        "orchestrator": c.orchestrates(),
        "agent": c.rung,
        "hosted": c.hosted,
        "sent_tokens": c.sent.iter().map(|(k, n)| json!({"section": k, "tokens": n})).collect::<Vec<_>>(),
        "input_tokens": c.input,
        "cached_tokens": c.cached,
        "output_tokens": c.tokens,
        "rounds": c.rounds,
        "cost_usd": c.cost,
        "queued_seconds": c.queued,
        "seconds": c.secs,
        "ok": c.ok,
    })
}

// Appends the record to the log when it is enabled; a write that fails is dropped so the command still succeeds.
pub fn write(rec: &Value) {
    let Some(path) = path() else { return };
    if append(&path, rec).is_err() {}
}

// Appends one line to the private log, creating its folder and rotating a full file first.
pub fn append(path: &Path, rec: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    }
    if path.metadata().is_ok_and(|m| m.len() > ROTATE_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    writeln!(file, "{rec}")
}
