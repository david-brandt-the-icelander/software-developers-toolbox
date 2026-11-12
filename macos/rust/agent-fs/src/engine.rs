// The request dispatcher: parses one JSON request, runs the operation and wraps the reply.
use crate::req::Request;
use crate::state::{Res, State};
use crate::{diff, list, outline, read, search};
use serde_json::{json, Value};

// One agent session: its state and the operations that use it.
pub struct Engine {
    state: State,
}

// Build the standard error reply.
fn error(message: String) -> Value {
    json!({"ok": false, "error": message})
}

impl Engine {
    // Create an engine with an empty session.
    pub fn new() -> Engine {
        Engine { state: State::new() }
    }

    // Run the operation a parsed request names.
    fn dispatch(&mut self, request: Request) -> Res<Value> {
        let st = &mut self.state;
        match request {
            Request::Read(a) => read::run(st, a),
            Request::Search(a) => search::run(st, a),
            Request::List(a) => list::list(st, a),
            Request::Stat(a) => list::stat(a),
            Request::Outline(a) => outline::run(st, a),
            Request::DiffSince(a) => diff::since(st, a),
        }
    }

    // Run one request object and return its reply with an `ok` flag.
    fn call_one(&mut self, input: Value) -> Value {
        let request: Request = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => return error(e.to_string()),
        };
        match self.dispatch(request) {
            Ok(mut v) => {
                v["ok"] = json!(true);
                v
            }
            Err(e) => error(e),
        }
    }

    // Run a request object, or each request of an array in order.
    pub fn call(&mut self, input: Value) -> Value {
        match input {
            Value::Array(items) => Value::Array(items.into_iter().map(|i| self.call_one(i)).collect()),
            single => self.call_one(single),
        }
    }
}

// Return true when a reply, or every reply of a batch, succeeded.
pub fn all_ok(reply: &Value) -> bool {
    match reply {
        Value::Array(items) => items.iter().all(all_ok),
        other => other["ok"] == json!(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Build a request from a JSON literal and run it.
    fn run(e: &mut Engine, v: Value) -> Value {
        e.call(v)
    }

    // Create a project directory with an old enough file so identity stamps are trusted.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\nfn beta() {}\n// TODO one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing\nTODO two\n").unwrap();
        dir
    }

    // A second identical read sends no content and a changed read sends only a diff.
    #[test]
    fn test_read_unchanged_then_diff() {
        let dir = project();
        let path = dir.path().join("a.rs").to_string_lossy().into_owned();
        let mut e = Engine::new();
        let first = run(&mut e, json!({"op": "read", "path": path}));
        assert_eq!(first["ok"], true);
        assert_eq!(first["total_lines"], 3);
        let again = run(&mut e, json!({"op": "read", "path": path}));
        assert_eq!(again["unchanged"], true);
        assert!(again.get("chunks").is_none());
        let big: String = (0..200).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &big).unwrap();
        run(&mut e, json!({"op": "read", "path": path}));
        std::fs::write(&path, big.replace("line 100\n", "changed\n")).unwrap();
        let diff = run(&mut e, json!({"op": "read", "path": path}));
        assert_eq!(diff["changed"], true);
        assert_eq!(diff["hunks"][0]["ins"], "changed\n");
        let fresh = run(&mut e, json!({"op": "read", "path": path, "fresh": true}));
        assert!(fresh.get("chunks").is_some());
    }

    // Ranges, truncation and if_none_match behave as documented.
    #[test]
    fn test_read_ranges_and_etag() {
        let dir = project();
        let path = dir.path().join("a.rs").to_string_lossy().into_owned();
        let mut e = Engine::new();
        let r = run(&mut e, json!({"op": "read", "path": path, "ranges": [[2, 3]], "max_lines": 1}));
        assert_eq!(r["chunks"][0]["text"], "fn beta() {}\n");
        assert_eq!(r["next_ranges"], json!([[3, 3]]));
        let etag = r["etag"].as_str().unwrap().to_string();
        let same = run(&mut e, json!({"op": "read", "path": path, "if_none_match": etag, "fresh": true}));
        assert_eq!(same["unchanged"], true);
        let bad = run(&mut e, json!({"op": "read", "path": path, "ranges": [[0, 1]]}));
        assert_eq!(bad["ok"], false);
    }

    // Search results are cached once files are old, and edits invalidate the cache.
    #[test]
    fn test_search_cache_and_invalidation() {
        let dir = project();
        std::thread::sleep(Duration::from_millis(2300));
        let root = dir.path().to_string_lossy().into_owned();
        let q = json!({"op": "search", "pattern": "TODO", "paths": [root]});
        let mut e = Engine::new();
        let first = run(&mut e, q.clone());
        assert_eq!(first["total_matches"], 2);
        assert_eq!(first["cached"], false);
        let second = run(&mut e, q.clone());
        assert_eq!(second["unchanged"], true);
        let third = run(&mut e, json!({"op": "search", "pattern": "TODO", "paths": [root], "fresh": true}));
        assert_eq!(third["cached"], true);
        std::fs::write(dir.path().join("b.txt"), "TODO two\nTODO three\n").unwrap();
        let fourth = run(&mut e, q);
        assert_eq!(fourth["total_matches"], 3);
        assert_eq!(fourth["cached"], false);
    }

    // Search supports context blocks, files_only and compact single-field output.
    #[test]
    fn test_search_shapes() {
        let dir = project();
        let root = dir.path().to_string_lossy().into_owned();
        let mut e = Engine::new();
        let ctx = run(&mut e, json!({"op": "search", "pattern": "alpha", "paths": [root], "context": 1}));
        assert_eq!(ctx["results"][0]["blocks"][0]["lines"].as_array().unwrap().len(), 2);
        let files =
            run(&mut e, json!({"op": "search", "pattern": "TODO", "paths": [root], "files_only": true}));
        assert_eq!(files["files"].as_array().unwrap().len(), 2);
        let lines =
            run(&mut e, json!({"op": "search", "pattern": "TODO", "paths": [root], "fields": ["line"]}));
        assert_eq!(lines["results"][0]["hits"], json!([3]));
        let cap = run(&mut e, json!({"op": "search", "pattern": "TODO", "paths": [root], "max_matches": 1}));
        assert_eq!(cap["truncated"], true);
    }

    // List, outline and diff_since work and unknown requests are rejected.
    #[test]
    fn test_list_outline_diff_since_errors() {
        let dir = project();
        let root = dir.path().to_string_lossy().into_owned();
        let mut e = Engine::new();
        let list = run(&mut e, json!({"op": "list", "path": root}));
        assert_eq!(list["total"], 2);
        let outline = run(&mut e, json!({"op": "outline", "paths": [root]}));
        assert_eq!(outline["files"][0]["symbols"][0][3], "alpha");
        let path = dir.path().join("b.txt").to_string_lossy().into_owned();
        let read = run(&mut e, json!({"op": "read", "path": path}));
        let etag = read["etag"].clone();
        std::fs::write(&path, "nothing\nTODO 2\n").unwrap();
        let d = run(&mut e, json!({"op": "diff_since", "path": path, "etag": etag}));
        assert_eq!(d["hunks"][0]["ins"], "TODO 2\n");
        assert_eq!(run(&mut e, json!({"op": "nope"}))["ok"], false);
        assert_eq!(run(&mut e, json!({"op": "read", "path": path, "extra": 1}))["ok"], false);
    }

    // Run a search on a warm engine and on a cold one and require equal results.
    fn same_as_cold(warm: &mut Engine, root: &str, pattern: &str, extra: Value) {
        let mut q = json!({"op": "search", "pattern": pattern, "paths": [root], "fresh": true});
        for (k, v) in extra.as_object().unwrap() {
            q[k] = v.clone();
        }
        let (a, b) = (warm.call(q.clone()), Engine::new().call(q));
        assert_eq!(a["results"], b["results"], "pattern {pattern}");
        assert_eq!(a["total_matches"], b["total_matches"], "pattern {pattern}");
    }

    // Indexed searches equal full scans across edits, additions and deletions.
    #[test]
    fn test_indexed_search_matches_full_scan() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..40 {
            let body = format!("fn item_{i}() {{}}\nlet Needle{} = {i};\nUPPER_{i} lower_{i}\n", i % 5);
            std::fs::write(dir.path().join(format!("f{i}.rs")), body).unwrap();
        }
        std::fs::write(dir.path().join("bin.dat"), b"Needle0\0binary").unwrap();
        std::thread::sleep(Duration::from_millis(2300));
        let root = dir.path().to_string_lossy().into_owned();
        let mut warm = Engine::new();
        let patterns =
            ["Needle3", "needle3", r"item_1\d", "UPPER_7|lower_9", r"\w+_12", "ne", "(?i)upper_2", "zzzz"];
        for round in 0..3 {
            for p in patterns {
                same_as_cold(&mut warm, &root, p, json!({}));
                same_as_cold(&mut warm, &root, p, json!({"ignore_case": true, "context": 1}));
            }
            match round {
                0 => std::fs::write(dir.path().join("f3.rs"), "brand new Needle3 text\n").unwrap(),
                1 => {
                    std::fs::remove_file(dir.path().join("f4.rs")).unwrap();
                    std::fs::write(dir.path().join("added.rs"), "Needle3 added\n").unwrap();
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(2300));
        }
    }

    // Batches return one reply per request and all_ok reflects failures.
    #[test]
    fn test_batch_and_all_ok() {
        let mut e = Engine::new();
        let out = e.call(json!([{"op": "stat", "path": "/"}, {"op": "read", "path": "/no/such"}]));
        assert_eq!(out.as_array().unwrap().len(), 2);
        assert!(!all_ok(&out));
        assert!(all_ok(&out[0]));
    }
}
