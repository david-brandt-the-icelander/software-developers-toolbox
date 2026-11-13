// MCP proxy: sits between a client and a server and shrinks the text of tool results and resource reads.
use crate::cmd::clean;
use crate::tokens;
use serde_json::{json, Value};
use std::collections::HashMap;

pub mod http;
pub mod stdio;

pub const USAGE: &str = "usage: zazzle proxy [--lossless] [--budget T] [--skip TOOL].. [--log] \
(-- CMD ARGS.. | --upstream URL [--listen ADDR])";
const DEFAULT_BUDGET: usize = 8000;
const DEFAULT_LISTEN: &str = "127.0.0.1:8808";

// How results are cleaned: the clean command's settings, tools left alone, and whether to log savings.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub lossy: bool,
    pub budget: usize,
    pub skip: Vec<String>,
    pub log: bool,
    pub store: Option<std::path::PathBuf>,
}

// Where the real MCP server is: a command speaking stdio, or a Streamable HTTP endpoint.
#[derive(Debug, PartialEq)]
pub enum Upstream {
    Command(Vec<String>),
    Http { url: String, listen: String },
}

// The parsed proxy command line.
#[derive(Debug, PartialEq)]
pub struct Opts {
    pub filter: Filter,
    pub upstream: Upstream,
}

// What a client request asked for, so its response is cleaned the right way.
#[derive(Debug, Clone, PartialEq)]
enum Asked {
    Tool(String),
    Resource,
}

// Client requests still waiting for their responses, keyed by JSON-RPC id.
#[derive(Debug, Default)]
pub struct Pending(HashMap<String, Asked>);

// One cleaned response: what it answered and its text size in bytes before and after.
#[derive(Debug, PartialEq)]
pub struct Saved {
    pub what: String,
    pub before: usize,
    pub after: usize,
}

// Takes the value after a flag.
fn value(it: &mut std::slice::Iter<String>, flag: &str) -> Result<String, String> {
    it.next().cloned().ok_or_else(|| format!("{flag} needs a value"))
}

// Parses the arguments after `zazzled proxy`.
pub fn parse(args: &[String]) -> Result<Opts, String> {
    let mut filter = Filter {
        lossy: true,
        budget: DEFAULT_BUDGET,
        skip: Vec::new(),
        log: false,
        store: crate::outputs::dir(),
    };
    let (mut url, mut listen, mut command) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => command = Some(it.by_ref().cloned().collect::<Vec<_>>()),
            "--lossless" => filter.lossy = false,
            "--log" => filter.log = true,
            "--skip" => filter.skip.push(value(&mut it, a)?),
            "--upstream" => url = Some(value(&mut it, a)?),
            "--listen" => listen = Some(value(&mut it, a)?),
            "--budget" => {
                filter.budget =
                    value(&mut it, a)?.parse().map_err(|_| "--budget needs a number".to_string())?;
                if filter.budget == 0 {
                    return Err("--budget must be at least 1".into());
                }
            }
            other => return Err(format!("unknown proxy option '{other}'; {USAGE}")),
        }
    }
    let upstream = match (command, url) {
        (Some(c), None) if !c.is_empty() && listen.is_none() => Upstream::Command(c),
        (None, Some(url)) => Upstream::Http { url, listen: listen.unwrap_or_else(|| DEFAULT_LISTEN.into()) },
        _ => return Err(format!("give either -- CMD ARGS.. or --upstream URL; {USAGE}")),
    };
    Ok(Opts { filter, upstream })
}

// The messages in a JSON-RPC payload: the message itself, or each element of a batch.
fn each_mut(msg: &mut Value) -> Vec<&mut Value> {
    match msg {
        Value::Array(items) => items.iter_mut().collect(),
        one => vec![one],
    }
}

impl Pending {
    // Records the tools/call and resources/read requests in a client payload; anything else is ignored.
    pub fn note_text(&mut self, text: &str) {
        let Ok(mut msg) = serde_json::from_str::<Value>(text) else { return };
        for m in each_mut(&mut msg) {
            let Some(id) = m.get("id") else { continue };
            let asked = match m.get("method").and_then(Value::as_str) {
                Some("tools/call") => {
                    Asked::Tool(m.pointer("/params/name").and_then(Value::as_str).unwrap_or("").to_string())
                }
                Some("resources/read") => Asked::Resource,
                _ => continue,
            };
            self.0.insert(id.to_string(), asked);
        }
    }

    // Returns true when no response is awaited, so server messages need not be parsed.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    // Removes and returns the request a server response answers; server requests carry a method and answer nothing.
    fn take(&mut self, m: &Value) -> Option<Asked> {
        if m.get("method").is_some() {
            return None;
        }
        self.0.remove(&m.get("id")?.to_string())
    }
}

impl Filter {
    // Replaces a text with its cleaned form when that saves at least one token; returns the sizes before and after.
    fn text(&self, s: &mut String) -> (usize, usize, Option<String>) {
        let before = s.len();
        let (out, steps) = clean::shrink_steps(s, self.lossy, self.budget);
        if tokens::from_bytes(out.len()) >= tokens::from_bytes(before) {
            return (before, before, None);
        }
        let original = std::mem::replace(s, out);
        (before, s.len(), self.keep(s, &original, clean::cuts(&steps)))
    }

    // Stores a shortened text with its original for read-again; returns the note that names its id, or why it was not stored.
    fn keep(&self, shown: &str, original: &str, cut: bool) -> Option<String> {
        let dir = self.store.as_deref()?;
        let id = crate::outputs::id_of(shown, original);
        Some(match crate::outputs::save(dir, &id, shown, original, crate::outputs::limit()) {
            Ok(()) if cut => format!("[r:{id} shortened by zazzle; full text: read-again r:{id} --full]"),
            Ok(()) => format!("[r:{id} shortened by zazzle]"),
            Err(e) => format!("[shortened by zazzle; not stored for read-again: {e}]"),
        })
    }

    // Cleans the text items of a tools/call result (`content`) or resources/read result (`contents`).
    fn result(&self, result: &mut Value, asked: &Asked) -> (usize, usize) {
        let key = if *asked == Asked::Resource { "contents" } else { "content" };
        let Some(items) = result.get_mut(key).and_then(Value::as_array_mut) else { return (0, 0) };
        let (mut before, mut after, mut notes) = (0, 0, Vec::new());
        for item in items.iter_mut() {
            let uri = item.get("uri").cloned();
            let slot = match item.get("type").and_then(Value::as_str) {
                Some("resource") => item.pointer_mut("/resource/text"),
                Some("text") | None => item.get_mut("text"),
                Some(_) => None,
            };
            if let Some(Value::String(s)) = slot {
                let (b, a, note) = self.text(s);
                before += b;
                after += a;
                notes.extend(note.map(|n| match uri {
                    Some(uri) => json!({"uri": uri, "mimeType": "text/plain", "text": n}),
                    None => json!({"type": "text", "text": n}),
                }));
            }
        }
        items.extend(notes);
        (before, after)
    }

    // Cleans the responses in a server payload that answer recorded requests; structuredContent is never touched.
    fn rewrite(&self, pending: &mut Pending, msg: &mut Value) -> Vec<Saved> {
        let mut saved = Vec::new();
        for m in each_mut(msg) {
            let Some(asked) = pending.take(m) else { continue };
            let what = match &asked {
                Asked::Tool(name) if self.skip.contains(name) => continue,
                Asked::Tool(name) => format!("tools/call {name}"),
                Asked::Resource => "resources/read".to_string(),
            };
            let Some(result) = m.get_mut("result") else { continue };
            let (before, after) = self.result(result, &asked);
            if after < before {
                saved.push(Saved { what, before, after });
            }
        }
        saved
    }

    // Cleans one serialized server payload; None means forward the original bytes unchanged.
    pub fn rewrite_text(&self, pending: &mut Pending, text: &str) -> Option<String> {
        if pending.is_empty() {
            return None;
        }
        let mut msg: Value = serde_json::from_str(text).ok()?;
        let saved = self.rewrite(pending, &mut msg);
        if saved.is_empty() {
            return None;
        }
        if self.log {
            for s in &saved {
                let (b, a) = (tokens::from_bytes(s.before), tokens::from_bytes(s.after));
                eprintln!("zazzled proxy: {} ~{b}→~{a} tokens", s.what);
            }
        }
        Some(msg.to_string())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use serde_json::json;

    // Converts string slices into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // A filter with the default settings.
    fn filter() -> Filter {
        Filter { lossy: true, budget: DEFAULT_BUDGET, skip: Vec::new(), log: false, store: None }
    }

    // Text that clean shrinks: pretty JSON.
    fn pretty() -> String {
        serde_json::to_string_pretty(&json!({"items": [1, 2, 3], "name": "x"})).expect("json")
    }

    // Checks both upstream forms and every option.
    #[test]
    fn test_parse_forms() {
        let o = parse(&v(&["--lossless", "--log", "--skip", "a", "--budget", "50", "--", "srv", "-x"]))
            .expect("opts");
        assert_eq!(o.upstream, Upstream::Command(v(&["srv", "-x"])));
        assert_eq!(
            o.filter,
            Filter { lossy: false, budget: 50, skip: v(&["a"]), log: true, store: crate::outputs::dir() }
        );
        let o = parse(&v(&["--upstream", "https://h/mcp"])).expect("opts");
        assert_eq!(o.upstream, Upstream::Http { url: "https://h/mcp".into(), listen: DEFAULT_LISTEN.into() });
        assert_eq!(o.filter, Filter { store: crate::outputs::dir(), ..filter() });
        let o = parse(&v(&["--upstream", "u", "--listen", "127.0.0.1:1"])).expect("opts");
        assert_eq!(o.upstream, Upstream::Http { url: "u".into(), listen: "127.0.0.1:1".into() });
    }

    // Checks the one-line errors for bad command lines.
    #[test]
    fn test_parse_errors() {
        let err = |a: &[&str]| parse(&v(a)).unwrap_err();
        assert!(err(&[]).starts_with("give either -- CMD ARGS.. or --upstream URL"));
        assert!(err(&["--"]).starts_with("give either"));
        assert!(err(&["--upstream", "u", "--", "x"]).starts_with("give either"));
        assert!(err(&["--listen", "a", "--", "x"]).starts_with("give either"));
        assert_eq!(err(&["--skip"]), "--skip needs a value");
        assert_eq!(err(&["--budget", "x"]), "--budget needs a number");
        assert_eq!(err(&["--budget", "0"]), "--budget must be at least 1");
        assert!(err(&["--what"]).starts_with("unknown proxy option '--what'; usage:"));
    }

    // Checks only tool calls and resource reads with ids are recorded, from single messages and batches.
    #[test]
    fn test_note_text() {
        let mut p = Pending::default();
        p.note_text(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get"}}"#);
        p.note_text(r#"[{"id":"r","method":"resources/read"},{"id":2,"method":"tools/list"},{"method":"tools/call"}]"#);
        p.note_text(r#"{"id":3,"method":"tools/call"}"#);
        p.note_text("not json");
        assert_eq!(p.0.len(), 3);
        assert_eq!(p.0.get("1"), Some(&Asked::Tool("get".into())));
        assert_eq!(p.0.get("\"r\""), Some(&Asked::Resource));
        assert_eq!(p.0.get("3"), Some(&Asked::Tool(String::new())));
    }

    // Checks a tool result's text, embedded resource text and a resource read are cleaned, other items kept.
    #[test]
    fn test_rewrite_cleans_results() {
        let f = filter();
        let mut p = Pending::default();
        p.note_text(
            r#"[{"id":1,"method":"tools/call","params":{"name":"t"}},{"id":2,"method":"resources/read"}]"#,
        );
        let msg = json!([
            {"id": 1, "result": {"content": [
                {"type": "text", "text": pretty()},
                {"type": "resource", "resource": {"uri": "f", "text": pretty()}},
                {"type": "image", "data": "x"},
                {"type": "text", "text": "short\n"}
            ], "structuredContent": {"keep": "  as is  "}}},
            {"id": 2, "result": {"contents": [{"uri": "f", "text": pretty()}]}}
        ]);
        let out: Value =
            serde_json::from_str(&f.rewrite_text(&mut p, &msg.to_string()).expect("changed")).expect("json");
        let min = r#"{"items":[1,2,3],"name":"x"}"#;
        assert_eq!(out[0]["result"]["content"][0]["text"], min);
        assert_eq!(out[0]["result"]["content"][1]["resource"]["text"], min);
        assert_eq!(out[0]["result"]["content"][2], json!({"type": "image", "data": "x"}));
        assert_eq!(out[0]["result"]["content"][3]["text"], "short\n");
        assert_eq!(out[0]["result"]["structuredContent"]["keep"], "  as is  ");
        assert_eq!(out[1]["result"]["contents"][0]["text"], min);
        assert!(p.is_empty());
    }

    // Checks a shortened text is stored with its original, and its id is added as its own item, so JSON text stays JSON.
    #[test]
    fn test_rewrite_stores_with_id() {
        let t = crate::testutil::TempDir::new();
        let f = Filter { store: Some(t.path().to_path_buf()), ..filter() };
        let mut p = Pending::default();
        p.note_text(
            r#"[{"id":1,"method":"tools/call","params":{"name":"t"}},{"id":2,"method":"resources/read"}]"#,
        );
        let msg = json!([
            {"id": 1, "result": {"content": [{"type": "text", "text": pretty()}]}},
            {"id": 2, "result": {"contents": [{"uri": "f", "text": pretty()}]}}
        ]);
        let out: Value =
            serde_json::from_str(&f.rewrite_text(&mut p, &msg.to_string()).expect("changed")).expect("json");
        let note = out[0]["result"]["content"][1]["text"].as_str().expect("note");
        assert!(note.starts_with("[r:") && note.ends_with(" shortened by zazzle]"), "{note}");
        let stored = crate::outputs::load(t.path(), &note[3..13]).expect("stored");
        assert_eq!((stored.shown.as_str(), stored.full), (r#"{"items":[1,2,3],"name":"x"}"#, pretty()));
        assert_eq!(out[1]["result"]["contents"][1]["uri"], "f");
        assert!(out[1]["result"]["contents"][1]["text"].as_str().expect("note").starts_with("[r:"));
        let cut = Filter { budget: 5, ..f.clone() };
        p.note_text(r#"{"id":3,"method":"tools/call","params":{"name":"t"}}"#);
        let long = (0..200).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let msg = json!({"id": 3, "result": {"content": [{"type": "text", "text": long}]}});
        let out: Value = serde_json::from_str(&cut.rewrite_text(&mut p, &msg.to_string()).expect("changed"))
            .expect("json");
        assert!(out["result"]["content"][1]["text"]
            .as_str()
            .expect("note")
            .contains("full text: read-again r:"));
        let broken = Filter { store: Some(t.path().join("no/such\0dir")), ..filter() };
        p.note_text(r#"{"id":4,"method":"tools/call","params":{"name":"t"}}"#);
        let msg = json!({"id": 4, "result": {"content": [{"type": "text", "text": pretty()}]}});
        let out: Value =
            serde_json::from_str(&broken.rewrite_text(&mut p, &msg.to_string()).expect("changed"))
                .expect("json");
        assert!(out["result"]["content"][1]["text"]
            .as_str()
            .expect("note")
            .starts_with("[shortened by zazzle; not stored"));
    }

    // Checks the cases left untouched: nothing pending, not JSON, unknown ids, server requests, skipped tools, errors.
    #[test]
    fn test_rewrite_leaves_others() {
        let f = Filter { skip: v(&["skipme"]), log: true, ..filter() };
        let mut p = Pending::default();
        let body = json!({"content": [{"type": "text", "text": pretty()}]});
        assert_eq!(f.rewrite_text(&mut p, &json!({"id": 1, "result": body}).to_string()), None);
        p.note_text(
            r#"[{"id":1,"method":"tools/call","params":{"name":"skipme"}},{"id":2,"method":"tools/call"}]"#,
        );
        p.note_text(r#"{"id":3,"method":"tools/call"}"#);
        assert_eq!(f.rewrite_text(&mut p, "not json"), None);
        assert_eq!(f.rewrite_text(&mut p, &json!({"id": 9, "result": body}).to_string()), None);
        assert_eq!(
            f.rewrite_text(&mut p, &json!({"id": 2, "method": "sampling/createMessage"}).to_string()),
            None
        );
        assert_eq!(f.rewrite_text(&mut p, &json!({"id": 1, "result": body}).to_string()), None);
        assert_eq!(f.rewrite_text(&mut p, &json!({"id": 2, "error": {"code": 1}}).to_string()), None);
        assert_eq!(f.rewrite_text(&mut p, &json!({"id": 3, "result": {"other": 1}}).to_string()), None);
        assert!(p.is_empty());
        p.note_text(r#"{"id":4,"method":"tools/call","params":{"name":"logged"}}"#);
        assert!(f.rewrite_text(&mut p, &json!({"id": 4, "result": body}).to_string()).is_some());
    }
}
