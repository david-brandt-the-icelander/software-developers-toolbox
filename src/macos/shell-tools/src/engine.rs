// The JSON request dispatcher: runs one request or a batch for a session and wraps each reply with an `ok` flag.
use crate::host::Host;
use crate::req::Request;
use crate::state::{Res, Sent, Shared, State};
use crate::{diff, list, outline, read, search};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

const MAX_SESSIONS: usize = 64;

// Where a request runs: the caller's working directory and the daemon services.
pub struct Ctx<'a> {
    pub cwd: &'a Path,
    pub host: &'a dyn Host,
}

// The memory behind every session's JSON requests: what each was sent, and what they share.
#[derive(Default)]
pub struct Engine {
    shared: Shared,
    sessions: HashMap<String, (u64, Sent)>,
    clock: u64,
}

// Builds the standard error reply.
fn error(message: String) -> Value {
    json!({"ok": false, "error": message})
}

// Returns a session's sent table, creating it and dropping the least recently used session when full.
fn session<'a>(sessions: &'a mut HashMap<String, (u64, Sent)>, clock: u64, key: &str) -> &'a mut Sent {
    if !sessions.contains_key(key) && sessions.len() >= MAX_SESSIONS {
        let oldest =
            sessions.iter().min_by_key(|(_, s)| s.0).map(|(k, _)| k.clone()).expect("sessions are full");
        sessions.remove(&oldest);
    }
    let entry = sessions.entry(key.to_string()).or_default();
    entry.0 = clock;
    &mut entry.1
}

// Runs the operation a parsed request names.
fn dispatch(st: &mut State, ctx: &Ctx, request: Request) -> Res<Value> {
    match request {
        Request::Read(a) => read::run(st, ctx, a),
        Request::Search(a) => search::run(st, ctx, a),
        Request::List(a) => list::list(st, ctx, a),
        Request::Stat(a) => list::stat(ctx, a),
        Request::Outline(a) => outline::run(st, ctx, a),
        Request::DiffSince(a) => diff::since(st, ctx, a),
    }
}

// Runs one request object and returns its reply with an `ok` flag.
fn call_one(st: &mut State, ctx: &Ctx, input: Value) -> Value {
    let request: Request = match serde_json::from_value(input) {
        Ok(r) => r,
        Err(e) => return error(e.to_string()),
    };
    match dispatch(st, ctx, request) {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v
        }
        Err(e) => error(e),
    }
}

impl Engine {
    // Runs a request object, or each request of an array in order, for a session; the empty session remembers nothing.
    pub fn call(&mut self, session: &str, ctx: &Ctx, input: Value) -> Value {
        self.clock += 1;
        let mut scratch = Sent::default();
        let sent = match session {
            "" => &mut scratch,
            key => self::session(&mut self.sessions, self.clock, key),
        };
        let mut st = State { shared: &mut self.shared, sent };
        match input {
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|i| call_one(&mut st, ctx, i)).collect())
            }
            single => call_one(&mut st, ctx, single),
        }
    }

    // Forgets every session whose key starts with the prefix; returns how many.
    pub fn forget_prefix(&mut self, prefix: &str) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|k, _| !k.starts_with(prefix));
        before - self.sessions.len()
    }
}

// Returns true when a reply, or every reply of a batch, succeeded.
pub fn all_ok(reply: &Value) -> bool {
    match reply {
        Value::Array(items) => items.iter().all(all_ok),
        other => other["ok"] == json!(true),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::host::testing::Plain;

    // Runs a request for a session in a directory.
    fn run(e: &mut Engine, session: &str, cwd: &Path, v: Value) -> Value {
        e.call(session, &Ctx { cwd, host: &Plain }, v)
    }

    // Creates a project directory with a Rust file and a text file.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\nfn beta() {}\n// TODO one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing\nTODO two\n").unwrap();
        dir
    }

    // A second identical read sends no content, a changed read sends only a diff, and fresh forces content.
    #[test]
    fn test_read_unchanged_then_diff() {
        let dir = project();
        let mut e = Engine::default();
        let read = |e: &mut Engine, extra: Value| {
            let mut q = json!({"op": "read", "path": "a.rs"});
            q.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            run(e, "s", dir.path(), q)
        };
        let first = read(&mut e, json!({}));
        assert_eq!((first["ok"].as_bool(), first["total_lines"].as_u64()), (Some(true), Some(3)));
        let again = read(&mut e, json!({}));
        assert_eq!(again["unchanged"], true);
        let big: String = (0..200).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("a.rs"), &big).unwrap();
        read(&mut e, json!({}));
        std::fs::write(dir.path().join("a.rs"), big.replace("line 100\n", "changed\n")).unwrap();
        let diff = read(&mut e, json!({}));
        assert_eq!(diff["hunks"][0]["ins"], "changed\n");
        assert!(read(&mut e, json!({"fresh": true})).get("chunks").is_some());
    }

    // Sessions are independent, and the empty session never answers unchanged.
    #[test]
    fn test_sessions_are_separate() {
        let dir = project();
        let mut e = Engine::default();
        let q = json!({"op": "read", "path": "b.txt"});
        run(&mut e, "a", dir.path(), q.clone());
        assert_eq!(run(&mut e, "a", dir.path(), q.clone())["unchanged"], true);
        assert!(run(&mut e, "b", dir.path(), q.clone()).get("chunks").is_some());
        assert!(run(&mut e, "", dir.path(), q.clone()).get("chunks").is_some());
        assert!(run(&mut e, "", dir.path(), q.clone()).get("chunks").is_some());
        assert_eq!(e.forget_prefix("a"), 1);
        assert!(run(&mut e, "a", dir.path(), q).get("chunks").is_some());
    }

    // The least recently used session is dropped when too many are kept.
    #[test]
    fn test_session_eviction() {
        let dir = project();
        let mut e = Engine::default();
        let q = json!({"op": "stat", "path": "b.txt"});
        for i in 0..=MAX_SESSIONS {
            run(&mut e, &format!("s{i}"), dir.path(), q.clone());
        }
        assert_eq!(e.sessions.len(), MAX_SESSIONS);
        assert!(!e.sessions.contains_key("s0"));
    }

    // Every operation runs through the dispatcher, and malformed requests are rejected.
    #[test]
    fn test_every_op_and_errors() {
        let dir = project();
        let mut e = Engine::default();
        let mut go = |v: Value| run(&mut e, "s", dir.path(), v);
        assert_eq!(go(json!({"op": "list"}))["total"], 2);
        assert_eq!(go(json!({"op": "ls", "path": "."}))["total"], 2);
        assert_eq!(go(json!({"op": "find", "pattern": "TODO"}))["total_matches"], 2);
        assert_eq!(go(json!({"op": "outline", "paths": ["a.rs"]}))["files"][0]["symbols"][0][3], "alpha");
        let etag = go(json!({"op": "stat", "path": "b.txt", "hash": true}))["etag"].clone();
        std::fs::write(dir.path().join("b.txt"), "nothing\nTODO 2\n").unwrap();
        let read = go(json!({"op": "read", "path": "b.txt"}));
        assert!(read["etag"] != etag);
        let d = go(json!({"op": "diff_since", "path": "b.txt", "etag": etag}));
        assert_eq!(d["ok"], false, "the stat etag was never stored as a diff base");
        assert_eq!(go(json!({"op": "nope"}))["ok"], false);
        assert_eq!(go(json!({"op": "read", "path": "b.txt", "extra": 1}))["ok"], false);
    }

    // Batches return one reply per request and all_ok reflects failures.
    #[test]
    fn test_batch_and_all_ok() {
        let mut e = Engine::default();
        let out = run(
            &mut e,
            "",
            Path::new("/"),
            json!([{"op": "stat", "path": "/"}, {"op": "read", "path": "/no/such"}]),
        );
        assert_eq!(out.as_array().unwrap().len(), 2);
        assert!(!all_ok(&out));
        assert!(all_ok(&out[0]));
    }
}
