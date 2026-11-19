// The call and stat commands: shell-tools' JSON operations run through the daemon's walk cache, find scan and outlines, with memory per session.
use super::find::{self, Plan};
use super::{Ctx, Output};
use crate::args::{self, flag, Flag};
use crate::engine::Engine;
use crate::pool::{cpus, par_map};
use crate::walk::{self, WalkOpts};
use regex::bytes::Regex;
use serde_json::{json, Value};
use shell_tools::engine::{self as calls, all_ok};
use shell_tools::host::{Host, Matches, Symbol};
use shell_tools::req::StatArgs;
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: call JSON | call - (one request object or an array of them)";
const STAT_SPEC: &[Flag] = &[flag("hash", '\0')];
const STAT_USAGE: &str = "usage: stat PATH.. [--hash]";

// The daemon services the JSON operations run on.
struct DaemonHost<'a> {
    engine: &'a Engine,
}

impl Host for DaemonHost<'_> {
    // Lists files from the walk cache, the same listing find and ls use.
    fn files(&self, roots: &[PathBuf], globs: &[String], no_ignore: bool) -> Result<Vec<PathBuf>, String> {
        walk::collect(roots, &WalkOpts { no_ignore, globs: globs.to_vec() }, &self.engine.walks)
    }

    // Lists directories from the walk cache.
    fn dirs(&self, root: &Path, no_ignore: bool) -> Result<Vec<PathBuf>, String> {
        walk::dirs(root, no_ignore, &self.engine.walks)
    }

    // Scans with find's engine, which skips binary files and files over 2 MiB; columns become 1-based.
    fn search(&self, files: &[PathBuf], re: &Regex, max: usize) -> Vec<Matches> {
        let plan = Plan { engine: self.engine, re: re.clone(), max, context: 0, files_only: false };
        find::search_all(files, &plan, cpus())
            .into_iter()
            .filter(|r| r.total > 0)
            .map(|r| Matches {
                path: files[r.idx].clone(),
                count: r.total,
                hits: r.hits.iter().map(|h| (h.line, h.col + 1)).collect(),
            })
            .collect()
    }

    // Outlines with the daemon's cached outlines, only files whose language is worth outlining.
    fn outline(&self, files: &[PathBuf]) -> Vec<Option<Vec<Symbol>>> {
        par_map(files, cpus(), |_, f| {
            if !crate::outline::worth_outlining(f) {
                return None;
            }
            let loaded = self.engine.load(f).ok().filter(|l| !l.meta.binary)?;
            let syms = self.engine.outline(f, &loaded);
            let rows = syms.iter().map(|s| Symbol {
                start: s.start,
                end: s.end,
                depth: s.depth,
                kind: s.kind.to_string(),
                name: s.name.clone(),
            });
            Some(rows.collect())
        })
    }
}

// Runs requests for the caller's session; the empty session remembers nothing between calls.
fn answer(ctx: &Ctx, input: Value) -> Value {
    let host = DaemonHost { engine: ctx.engine };
    let lib = calls::Ctx { cwd: &ctx.cwd, host: &host };
    let mut engine = ctx.engine.calls.lock().unwrap_or_else(|e| e.into_inner());
    engine.call(&ctx.session, &lib, input)
}

// Runs the call command: one JSON request or an array of them, from the argument or standard input.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let text = match argv {
        [json] if json != "-" => json.clone(),
        [] | [_] => String::from_utf8_lossy(&ctx.read_stdin()?).into_owned(),
        _ => return Err(USAGE.into()),
    };
    let reply = match serde_json::from_str::<Value>(&text) {
        Ok(input) => answer(ctx, input),
        Err(e) => json!({"ok": false, "error": format!("invalid JSON: {e}")}),
    };
    let status = if all_ok(&reply) { 0 } else { 1 };
    Ok(Output { status, ..Output::plain(reply.to_string()) })
}

// Formats Unix seconds as a UTC date and time.
fn utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

// Renders one stat reply as a line: path, kind, size, modification time and the sha read shows.
fn stat_line(shown: &str, v: &Value) -> String {
    if v["exists"] == false {
        return format!("{shown} missing");
    }
    let mut line = format!(
        "{shown} {} {}B {}",
        v["kind"].as_str().unwrap_or("?"),
        v["size"],
        utc(v["mtime"].as_i64().unwrap_or(0))
    );
    if let Some(etag) = v["etag"].as_str().and_then(|e| e.strip_prefix("b3:")) {
        line.push_str(&format!(" sha {}", &etag[..8]));
    }
    line
}

// Runs the stat command: one line per path, through the same stat the JSON operation uses.
pub fn stat(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(STAT_SPEC, argv).map_err(|e| format!("{e}; {STAT_USAGE}"))?;
    if p.positional.is_empty() {
        return Err(STAT_USAGE.into());
    }
    let host = DaemonHost { engine: ctx.engine };
    let lib = calls::Ctx { cwd: &ctx.cwd, host: &host };
    let mut lines = Vec::new();
    for path in &p.positional {
        let v = shell_tools::list::stat(&lib, StatArgs { path: path.clone(), hash: p.has("hash") })?;
        lines.push(stat_line(path, &v));
    }
    Ok(Output::ok(lines.join("\n")))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{run_cmd, run_cmd_with, TempDir};
    use std::cell::RefCell;

    // Builds a project with code, text, a binary, an ignored file and a hidden folder.
    fn project() -> TempDir {
        let t = TempDir::new();
        t.write(
            "src/a.rs",
            b"struct A;\n\nimpl A {\n    fn new() -> A {\n        A\n    }\n}\n// TODO one\n",
        );
        t.write("notes.txt", b"nothing\nTODO two TODO\n");
        t.write("bin.dat", b"TODO\0binary");
        t.write("skip.log", b"TODO\n");
        t.write(".gitignore", b"*.log\n");
        t.write(".hid/x.txt", b"TODO hidden\n");
        t
    }

    // Runs call with one JSON argument in a session and parses the reply.
    fn call(e: &Engine, cwd: &Path, session: &str, v: Value) -> (Value, u8) {
        let o = run_cmd_with(e, cwd, session, &["call", &v.to_string()]);
        (serde_json::from_str(&o.text).expect("json reply"), o.status)
    }

    // Checks read, search, list, outline and stat answer through the daemon's services.
    #[test]
    fn test_call_ops() {
        let t = project();
        let e = Engine::new();
        let go = |v: Value| call(&e, t.path(), "", v).0;
        let read = go(json!({"op": "read", "path": "src/a.rs", "ranges": [[1, 1]]}));
        assert_eq!(read["chunks"][0]["text"], "struct A;\n");
        let found = go(json!({"op": "search", "pattern": "TODO"}));
        assert_eq!((found["files_matched"].as_u64(), found["total_matches"].as_u64()), (Some(2), Some(2)));
        assert_eq!(found["results"][0]["path"], "notes.txt");
        assert_eq!(found["results"][1]["hits"][0], json!({"line": 8, "text": "// TODO one"}));
        let cols = go(json!({"op": "find", "pattern": "two", "paths": ["notes.txt"], "fields": ["col"]}));
        assert_eq!(cols["results"][0]["hits"], json!([6]));
        let all = go(
            json!({"op": "search", "pattern": "TODO", "hidden": true, "no_ignore": true, "files_only": true}),
        );
        assert_eq!(all["files"], json!([".hid/x.txt", "notes.txt", "skip.log", "src/a.rs"]));
        let list = go(json!({"op": "list"}));
        assert_eq!(list["entries"], json!(["bin.dat", "notes.txt", "src/"]));
        let deep = go(json!({"op": "ls", "glob": ["*.rs"], "long": true}));
        assert_eq!(deep["entries"][0][0], "src/a.rs");
        let outline = go(json!({"op": "outline", "paths": ["."]}));
        assert_eq!(
            outline["files"],
            json!([{"path": "src/a.rs", "symbols": [[1, 1, "struct", "A"], [3, 7, "impl", "A"], [4, 6, "fn", "A.new"]]}])
        );
        let none = go(json!({"op": "outline", "paths": ["notes.txt", "bin.dat"]}));
        assert_eq!(none["files"], json!([]));
        assert_eq!(go(json!({"op": "stat", "path": "src", "hash": true}))["kind"], "dir");
        assert!(go(json!({"op": "list", "path": "nope"}))["error"].as_str().expect("error").contains("nope"));
    }

    // Checks a session gets unchanged markers and diffs across calls, and forget-sessions clears it.
    #[test]
    fn test_call_session_memory() {
        let t = project();
        let e = Engine::new();
        let q = json!({"op": "read", "path": "notes.txt"});
        let first = call(&e, t.path(), "S/main", q.clone()).0;
        assert!(first.get("chunks").is_some());
        assert_eq!(call(&e, t.path(), "S/main", q.clone()).0["unchanged"], true);
        assert!(call(&e, t.path(), "", q.clone()).0.get("chunks").is_some());
        let body: String = (0..100).map(|i| format!("line {i}\n")).collect();
        t.write("notes.txt", body.as_bytes());
        let etag = call(&e, t.path(), "S/main", q.clone()).0["etag"].clone();
        t.write("notes.txt", body.replace("line 50\n", "changed\n").as_bytes());
        assert_eq!(call(&e, t.path(), "S/main", q.clone()).0["hunks"][0]["ins"], "changed\n");
        let since =
            call(&e, t.path(), "S/main", json!({"op": "diff_since", "path": "notes.txt", "etag": etag})).0;
        assert_eq!(since["hunks"][0]["del"], 1);
        run_cmd_with(&e, t.path(), "", &["forget-sessions", "S/"]);
        assert!(call(&e, t.path(), "S/main", q).0.get("chunks").is_some());
    }

    // Checks batches, failures, invalid JSON, standard input and usage.
    #[test]
    fn test_call_batch_errors_and_stdin() {
        let t = project();
        let e = Engine::new();
        let (batch, status) =
            call(&e, t.path(), "", json!([{"op": "stat", "path": "x"}, {"op": "read", "path": "x"}]));
        assert_eq!(
            (batch[0]["exists"].as_bool(), batch[1]["ok"].as_bool(), status),
            (Some(false), Some(false), 1)
        );
        let bad = run_cmd(t.path(), &["call", "{nope"]);
        assert!(
            bad.text.starts_with("{\"error\":\"invalid JSON") || bad.text.contains("invalid JSON"),
            "{}",
            bad.text
        );
        assert_eq!(bad.status, 1);
        assert_eq!(run_cmd(t.path(), &["call", "a", "b"]).text, format!("error: {USAGE}"));
        assert_eq!(run_cmd(t.path(), &["call"]).text, "error: no standard input available");
        let ctx = Ctx {
            roots: Vec::new(),
            engine: &e,
            cwd: t.path().to_path_buf(),
            session: String::new(),
            stdin: RefCell::new(Some(Box::new(&b"{\"op\":\"stat\",\"path\":\"notes.txt\"}"[..]))),
            caller: RefCell::new(None),
        };
        let o = run(&ctx, &["-".to_string()]).expect("stdin");
        assert_eq!((o.status, o.text.contains("\"kind\":\"file\"")), (0, true));
    }

    // Checks stat lines for a file with its sha, a directory and a missing path, and the usage errors.
    #[test]
    fn test_stat_command() {
        let t = project();
        crate::testutil::age(&t.path().join("notes.txt"));
        let o = run_cmd(t.path(), &["stat", "notes.txt", "src", "gone", "--hash"]);
        let lines: Vec<&str> = o.text.lines().collect();
        let sha = &shell_tools::fsio::etag(b"nothing\nTODO two TODO\n")[3..11];
        assert!(
            lines[0].starts_with("notes.txt file 22B ") && lines[0].ends_with(&format!(" sha {sha}")),
            "{}",
            lines[0]
        );
        assert!(lines[1].starts_with("src dir "), "{}", lines[1]);
        assert_eq!(lines[2], "gone missing");
        assert!(o.idable);
        assert_eq!(run_cmd(t.path(), &["stat"]).text, format!("error: {STAT_USAGE}"));
        assert!(run_cmd(t.path(), &["stat", "x", "--nope"]).text.contains(STAT_USAGE));
    }

    // Checks UTC formatting at the epoch, a leap day and before 1970.
    #[test]
    fn test_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400 + 3661), "2000-02-29T01:01:01Z");
        assert_eq!(utc(-1), "1969-12-31T23:59:59Z");
        assert_eq!(utc(1_763_510_400), "2025-11-19T00:00:00Z");
    }
}
