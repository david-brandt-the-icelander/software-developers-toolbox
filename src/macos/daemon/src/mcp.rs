// MCP stdio front end: exposes the zazzle commands as tools and forwards them to the daemon.
use crate::client;
use crate::proto::Request;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

// Anything that can run a command line and return its status and text.
pub trait Backend {
    // Runs the arguments as one zazzle command, under the given session when the hook named one.
    fn call(&self, args: Vec<String>, session: Option<&str>) -> Result<(u8, String), String>;

    // Records the workspace folders the client named, which later commands may write in.
    fn set_roots(&self, _roots: Vec<String>) {}

    // Hands the connection to an upgraded program before answering a request, when there is one; returns when there is none.
    fn upgrade(&self, _pending: &str, _offers_roots: bool) {}
}

// The real backend: a unix-socket client that starts the daemon on demand.
pub struct DaemonBackend {
    pub sock: PathBuf,
    pub cwd: String,
    pub session: String,
    pub roots: std::cell::RefCell<Vec<String>>,
}

impl Backend for DaemonBackend {
    // Sends the command to daemon in relay mode.
    fn call(&self, args: Vec<String>, session: Option<&str>) -> Result<(u8, String), String> {
        let session = crate::proto::join_session(session.unwrap_or(&self.session), &self.roots.borrow());
        let req = Request { cwd: self.cwd.clone(), session, args };
        client::call_autostart(&self.sock, &req, &client::spawn_daemon)
            .map_err(|e| format!("cannot reach daemon: {e}"))
    }

    // Keeps the client's roots for the following calls.
    fn set_roots(&self, roots: Vec<String>) {
        *self.roots.borrow_mut() = roots;
    }

    // Re-executes the upgraded daemon on this connection, so the client is served the installed version's tools.
    fn upgrade(&self, pending: &str, offers_roots: bool) {
        crate::mcp_upgrade::reexec_if_changed(pending, offers_roots, &self.session);
    }
}

// One exposed tool: how it is described and how its arguments become a command line.
pub(crate) struct Tool {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) schema: fn() -> Value,
    pub(crate) map: fn(&Value) -> Result<Vec<String>, String>,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "zazzle_find",
        description: "Regex search grouped by file with line numbers; capped and token-lean. Prefer over grep.",
        schema: find_schema,
        map: find_args,
    },
    Tool {
        name: "zazzle_read",
        description: "Read the literal source of files, line ranges (PATH or PATH:A-B, REV:PATH) or one named symbol with its docs and attributes, as numbered lines with the sha edits need; past the budget it says what is missing and how to continue.",
        schema: read_schema,
        map: read_args,
    },
    Tool {
        name: "zazzle_read_again",
        description: "Get an [r:ID] response again (the current version if it changed); clean, run and proxy outputs are kept an hour, full=true gives them uncut.",
        schema: read_again_schema,
        map: read_again_args,
    },
    Tool { name: "zazzle_ls", description: "Compact directory tree with file counts and line counts.", schema: ls_schema, map: ls_args },
    Tool {
        name: "zazzle_stat",
        description: "Whether paths exist, with kind, size, mtime and (hash) the sha zazzle_read shows.",
        schema: stat_schema,
        map: stat_args,
    },
    Tool {
        name: "zazzle_call",
        description: "JSON requests (one or an array): read, search, list (cursor paging), stat, outline, diff_since; replies carry etags, repeats in a session return unchanged, changed reads return hunks. See zazzle help call.",
        schema: call_schema,
        map: call_args,
    },
    Tool {
        name: "zazzle_diff",
        description: "What changed: per-file stats, then hunks labelled with their symbol; generated files skipped. Prefer over git diff.",
        schema: diff_schema,
        map: diff_args,
    },
    Tool {
        name: "zazzle_replace",
        description: "Regex replacement across files ($1 for groups); dry_run previews. Prefer over sed -i.",
        schema: replace_schema,
        map: replace_args,
    },
    Tool {
        name: "zazzle_outline",
        description: "Symbols (functions, types, headings) with line ranges for files or directories; read big files in pieces with zazzle_read.",
        schema: outline_schema,
        map: outline_args,
    },
    Tool {
        name: "zazzle_refs",
        description: "Where a symbol is defined and used: definitions first, each use with its enclosing symbol.",
        schema: refs_schema,
        map: refs_args,
    },
    Tool {
        name: "zazzle_prompt",
        description: "Ask the local open model (no API cost); to shrink another tool's output, give that tool `llm` instead.",
        schema: prompt_schema,
        map: prompt_args,
    },
    Tool {
        name: "zazzle_edit",
        description: "Edit one file: replace unique text or lines, or insert after a line (expect = the sha zazzle_read showed), or create a file. Writes stay in the repository, client roots and workspaces.",
        schema: edit_schema,
        map: edit_args,
    },
];

// Schema for zazzle_prompt.
fn prompt_schema() -> Value {
    object(
        json!({
            "text": {"type": "string", "description": "The question or task"},
            "effort": {"type": "string", "enum": ["low", "medium", "high"], "description": "low: no thinking; default: the saved one"}
        }),
        &["text"],
    )
}

// Turns zazzle_prompt arguments into a command line.
fn prompt_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["prompt".to_string()];
    if let Some(e) = get_str(v, "effort")? {
        a.extend(["--effort".to_string(), e]);
    }
    a.push(need_str(v, "text")?);
    Ok(a)
}

// Builds a JSON schema object from properties and required names.
pub(crate) fn object(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

// Schema for zazzle_diff.
fn diff_schema() -> Value {
    object(
        json!({
            "range": {"type": "string", "description": "BASE, BASE..HEAD or BASE...HEAD; default the working tree against HEAD"},
            "paths": {"type": "array", "items": {"type": "string"}},
            "names": {"type": "boolean", "description": "Only the file stats"},
            "budget": {"type": "integer"}
        }),
        &[],
    )
}

// Turns zazzle_diff arguments into a command line.
fn diff_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["diff".to_string()];
    a.extend(get_str(v, "range")?);
    a.extend(get_list(v, "paths")?);
    push_flag(&mut a, v, "names", "--names")?;
    push_num(&mut a, v, "budget", "--budget")?;
    Ok(a)
}

// Schema for zazzle_replace.
fn replace_schema() -> Value {
    object(
        json!({
            "pattern": {"type": "string"},
            "replacement": {"type": "string"},
            "paths": {"type": "array", "items": {"type": "string"}},
            "glob": {"type": "array", "items": {"type": "string"}},
            "dry_run": {"type": "boolean"},
            "fixed": {"type": "boolean", "description": "Pattern and replacement are plain text"}
        }),
        &["pattern", "replacement"],
    )
}

// Turns zazzle_replace arguments into a command line.
fn replace_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["replace".to_string(), need_str(v, "pattern")?, need_str(v, "replacement")?];
    a.extend(get_list(v, "paths")?);
    for g in get_list(v, "glob")? {
        a.extend(["-g".to_string(), g]);
    }
    push_flag(&mut a, v, "dry_run", "--dry-run")?;
    push_flag(&mut a, v, "fixed", "--fixed")?;
    Ok(a)
}

// Schema for zazzle_find.
fn find_schema() -> Value {
    object(
        json!({
            "pattern": {"type": "string", "description": "Regex (Rust syntax; no look-around)"},
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Files or directories; default is the working directory"},
            "glob": {"type": "array", "items": {"type": "string"}, "description": "Include globs; prefix ! to exclude"},
            "ignore_case": {"type": "boolean"}, "fixed": {"type": "boolean", "description": "Treat pattern as literal text"},
            "word": {"type": "boolean"}, "count": {"type": "boolean"}, "files_only": {"type": "boolean"},
            "no_ignore": {"type": "boolean", "description": "Also search gitignored files"},
            "code_only": {"type": "boolean", "description": "Skip tests and docs"},
            "context": {"type": "integer"}, "max": {"type": "integer", "description": "Matches shown (default 30, at most 40)"},
            "disable_session_deduping": {"type": "boolean"}
        }),
        &["pattern"],
    )
}

// Schema for zazzle_read.
fn read_schema() -> Value {
    object(
        json!({
            "paths": {"type": "array", "items": {"type": "string"}, "description": "PATH or PATH:A-B; optional with symbol"},
            "terse": {"type": "boolean", "description": "Opt-in lossy summary (folds imports, comments, logging; no sha); the default is the literal source"},
            "symbol": {"type": "string", "description": "One symbol with its docs: Namespace.Type.Member, Type.method or a::b; overloads Name(T1, T2) or #N; without paths, found in the index"},
            "around": {"type": "integer"}, "ctx": {"type": "integer"}, "head": {"type": "integer"}, "tail": {"type": "integer"},
            "budget": {"type": "integer", "description": "Token budget (default 6000, at most 16000)"},
            "anchors": {"type": "boolean"}, "full": {"type": "boolean", "description": "The largest budget, 16000"},
            "disable_session_deduping": {"type": "boolean"}
        }),
        &[],
    )
}

// Schema for zazzle_read_again.
fn read_again_schema() -> Value {
    object(
        json!({
            "id": {"type": "string", "description": "The id from a response's [r:ID] tag"},
            "full": {"type": "boolean", "description": "Uncut text of a clean, run or proxy output"},
            "lines": {"type": "string", "description": "With full: a line range A-B"},
            "budget": {"type": "integer", "description": "With full: token budget (default 2000, at most 2500)"}
        }),
        &["id"],
    )
}

// Schema for zazzle_ls.
fn ls_schema() -> Value {
    object(
        json!({
            "path": {"type": "string"}, "depth": {"type": "integer"}, "max": {"type": "integer", "description": "At most 60"},
            "no_ignore": {"type": "boolean"}, "disable_session_deduping": {"type": "boolean"}
        }),
        &[],
    )
}

// Schema for zazzle_stat.
fn stat_schema() -> Value {
    object(
        json!({
            "paths": {"type": "array", "items": {"type": "string"}},
            "hash": {"type": "boolean", "description": "Add each file's sha"}
        }),
        &["paths"],
    )
}

// Schema for zazzle_call.
fn call_schema() -> Value {
    object(
        json!({
            "requests": {"type": ["object", "array"], "description": "One request such as {\"op\":\"read\",\"path\":\"a.rs\",\"ranges\":[[1,40]]}, or an array of them"}
        }),
        &["requests"],
    )
}

// Schema for zazzle_outline.
fn outline_schema() -> Value {
    object(
        json!({
            "paths": {"type": "array", "items": {"type": "string"}}, "depth": {"type": "integer", "description": "Maximum nesting shown (default 3)"},
            "max": {"type": "integer"}, "disable_session_deduping": {"type": "boolean"}
        }),
        &["paths"],
    )
}

// Schema for zazzle_refs.
fn refs_schema() -> Value {
    object(
        json!({
            "symbol": {"type": "string", "description": "One literal name; Type.Member narrows"}, "also": {"type": "array", "items": {"type": "string"}, "description": "More literal names, answered together"}, "paths": {"type": "array", "items": {"type": "string"}},
            "glob": {"type": "array", "items": {"type": "string"}}, "max": {"type": "integer", "description": "At most 40"},
            "code_only": {"type": "boolean", "description": "Skip tests and docs"},
            "no_ignore": {"type": "boolean"}, "disable_session_deduping": {"type": "boolean"}
        }),
        &["symbol"],
    )
}

// Schema for zazzle_edit.
fn edit_schema() -> Value {
    object(
        json!({
            "path": {"type": "string"},
            "old": {"type": "string", "description": "Exact text to replace; must be unique unless all is true"},
            "new": {"type": "string", "description": "Replacement or inserted text"},
            "all": {"type": "boolean"}, "create": {"type": "boolean", "description": "Create the file with new as its content"},
            "lines": {"type": "string", "description": "Line range A-B to replace; also give expect or at"},
            "at": {"type": "string", "description": "Anchored range A#xyz-B#xyz from zazzle_read with anchors"},
            "after": {"type": "integer", "description": "Insert new after this line (0 = at the start); also give expect"},
            "expect": {"type": "string", "description": "The sha zazzle_read showed; every edit but create needs it"},
            "edits": {"type": "array", "description": "Several {old, new} pairs, applied in order in one write; none is written if one fails", "items": {"type": "object", "properties": {"old": {"type": "string"}, "new": {"type": "string"}}, "required": ["old", "new"]}}
        }),
        &["path"],
    )
}

// Reads an optional string argument.
pub(crate) fn get_str(v: &Value, key: &str) -> Result<Option<String>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("'{key}' must be a string")),
    }
}

// Reads a required string argument.
pub(crate) fn need_str(v: &Value, key: &str) -> Result<String, String> {
    get_str(v, key)?.ok_or_else(|| format!("'{key}' is required"))
}

// Reads an optional list of strings.
pub(crate) fn get_list(v: &Value, key: &str) -> Result<Vec<String>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str().map(str::to_string).ok_or_else(|| format!("'{key}' must be a list of strings"))
            })
            .collect(),
        Some(_) => Err(format!("'{key}' must be a list of strings")),
    }
}

// Pushes a boolean flag when the argument is true.
pub(crate) fn push_flag(out: &mut Vec<String>, v: &Value, key: &str, flag: &str) -> Result<(), String> {
    match v.get(key) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(()),
        Some(Value::Bool(true)) => {
            out.push(flag.into());
            Ok(())
        }
        Some(_) => Err(format!("'{key}' must be true or false")),
    }
}

// Pushes a numeric option when the argument is present, lowered to the ceiling an agent may ask for.
pub(crate) fn push_capped(
    out: &mut Vec<String>,
    v: &Value,
    key: &str,
    flag: &str,
    cap: u64,
) -> Result<(), String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(n) => {
            let n = n.as_u64().ok_or_else(|| format!("'{key}' must be a non-negative integer"))?;
            out.push(flag.into());
            out.push(n.min(cap).to_string());
            Ok(())
        }
    }
}

// Ceilings on what an agent may ask for: larger answers cost more than the extra call they save.
pub(crate) const FIND_MAX: u64 = 40;
pub(crate) const REFS_MAX: u64 = 40;
pub(crate) const LS_MAX: u64 = 60;
pub(crate) const READ_BUDGET: u64 = 2500;
pub(crate) const FIND_SYMBOL_LIMIT: u64 = 20;

// Pushes a numeric option when the argument is present.
pub(crate) fn push_num(out: &mut Vec<String>, v: &Value, key: &str, flag: &str) -> Result<(), String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(n) => {
            let n = n.as_u64().ok_or_else(|| format!("'{key}' must be a non-negative integer"))?;
            out.push(flag.into());
            out.push(n.to_string());
            Ok(())
        }
    }
}

// Maps zazzle_find arguments to a command line.
fn find_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["find".to_string()];
    for (k, f) in [
        ("ignore_case", "-i"),
        ("fixed", "-F"),
        ("word", "-w"),
        ("count", "-c"),
        ("files_only", "-l"),
        ("no_ignore", "-u"),
        ("code_only", "--code-only"),
        ("disable_session_deduping", "--disable-session-deduping"),
    ] {
        push_flag(&mut a, v, k, f)?;
    }
    push_num(&mut a, v, "context", "-C")?;
    push_capped(&mut a, v, "max", "-m", FIND_MAX)?;
    push_capped(&mut a, v, "limit", "-m", FIND_MAX)?;
    for g in get_list(v, "glob")? {
        a.push("-g".into());
        a.push(g);
    }
    a.push("--".into());
    a.push(need_str(v, "pattern")?);
    a.extend(get_list(v, "paths")?);
    Ok(a)
}

// Maps zazzle_read arguments to a command line.
fn read_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["read".to_string()];
    for (k, f) in [
        ("anchors", "--anchors"),
        ("source", "--source"),
        ("terse", "--terse"),
        ("full", "--full"),
        ("disable_session_deduping", "--disable-session-deduping"),
    ] {
        push_flag(&mut a, v, k, f)?;
    }
    for (k, f) in [("around", "--around"), ("ctx", "--ctx"), ("head", "--head"), ("tail", "--tail")] {
        push_num(&mut a, v, k, f)?;
    }
    push_capped(&mut a, v, "budget", "--budget", crate::cmd::read::MAX_BUDGET as u64)?;
    if let Some(sym) = get_str(v, "symbol")? {
        a.push("--symbol".into());
        a.push(sym);
    }
    let paths = get_list(v, "paths")?;
    if paths.is_empty() && !a.contains(&"--symbol".to_string()) {
        return Err("'paths' is required (or give symbol alone)".into());
    }
    a.push("--".into());
    a.extend(paths);
    Ok(a)
}

// Maps zazzle_ls arguments to a command line.
fn ls_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["ls".to_string()];
    push_flag(&mut a, v, "no_ignore", "-u")?;
    push_flag(&mut a, v, "disable_session_deduping", "--disable-session-deduping")?;
    push_num(&mut a, v, "depth", "-d")?;
    push_capped(&mut a, v, "max", "-m", LS_MAX)?;
    if let Some(p) = get_str(v, "path")? {
        a.push("--".into());
        a.push(p);
    }
    Ok(a)
}

// Maps zazzle_outline arguments to a command line.
fn outline_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["outline".to_string()];
    push_flag(&mut a, v, "disable_session_deduping", "--disable-session-deduping")?;
    push_num(&mut a, v, "depth", "-d")?;
    push_num(&mut a, v, "max", "-m")?;
    let paths = get_list(v, "paths")?;
    if paths.is_empty() {
        return Err("'paths' is required".into());
    }
    a.push("--".into());
    a.extend(paths);
    Ok(a)
}

// Maps zazzle_read_again arguments to a command line.
fn read_again_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["read-again".to_string(), need_str(v, "id")?];
    push_flag(&mut a, v, "full", "--full")?;
    push_capped(&mut a, v, "budget", "--budget", READ_BUDGET)?;
    if let Some(lines) = get_str(v, "lines")? {
        a.extend(["--lines".to_string(), lines]);
    }
    Ok(a)
}

// Maps zazzle_refs arguments to a command line.
fn refs_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["refs".to_string()];
    push_flag(&mut a, v, "no_ignore", "-u")?;
    push_flag(&mut a, v, "code_only", "--code-only")?;
    push_flag(&mut a, v, "disable_session_deduping", "--disable-session-deduping")?;
    push_capped(&mut a, v, "max", "-m", REFS_MAX)?;
    push_capped(&mut a, v, "limit", "-m", REFS_MAX)?;
    for g in get_list(v, "glob")? {
        a.push("-g".into());
        a.push(g);
    }
    for n in get_list(v, "also")? {
        a.extend(["--also".to_string(), n]);
    }
    a.push("--".into());
    a.push(need_str(v, "symbol")?);
    a.extend(get_list(v, "paths")?);
    Ok(a)
}

// Maps zazzle_stat arguments to a command line.
fn stat_args(v: &Value) -> Result<Vec<String>, String> {
    let paths = get_list(v, "paths")?;
    if paths.is_empty() {
        return Err("'paths' is required".into());
    }
    let mut a = vec!["stat".to_string()];
    push_flag(&mut a, v, "hash", "--hash")?;
    a.push("--".into());
    a.extend(paths);
    Ok(a)
}

// Maps zazzle_call arguments to a command line: the requests as one JSON argument.
fn call_args(v: &Value) -> Result<Vec<String>, String> {
    match v.get("requests") {
        Some(r @ (Value::Object(_) | Value::Array(_))) => Ok(vec!["call".to_string(), r.to_string()]),
        Some(Value::String(s)) => Ok(vec!["call".to_string(), s.clone()]),
        _ => Err("'requests' is required: a request object or an array of them".into()),
    }
}

// Maps zazzle_edit arguments to a command line.
fn edit_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["edit".to_string(), need_str(v, "path")?];
    push_flag(&mut a, v, "all", "--all")?;
    push_flag(&mut a, v, "create", "--create")?;
    push_num(&mut a, v, "after", "--after")?;
    for key in ["old", "new", "lines", "at", "expect"] {
        if let Some(s) = get_str(v, key)? {
            a.push(format!("--{key}"));
            a.push(s);
        }
    }
    for pair in v.get("edits").and_then(Value::as_array).into_iter().flatten() {
        for key in ["old", "new"] {
            a.push(format!("--{key}"));
            a.push(need_str(pair, key)?);
        }
    }
    Ok(a)
}

// Builds a JSON-RPC error response.
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

// Builds a JSON-RPC success response.
fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

// Answers initialize by agreeing a protocol version and telling the agent how to use zazzle.
fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": true}},
        "serverInfo": {"name": "zazzle", "version": crate::cmd::VERSION},
        "instructions": crate::setup::MCP_INSTRUCTIONS
    })
}

// Every exposed tool: the file tools here, the structure tools answered by csharp-indexer, then the zazz script tools.
fn all_tools() -> impl Iterator<Item = &'static Tool> {
    TOOLS
        .iter()
        .chain(crate::mcp_structure::TOOLS)
        .chain(crate::rules::TOOLS)
        .chain(crate::investigate::TOOLS)
        .chain(crate::brief::TOOLS)
        .chain(crate::zazz::mcptools::TOOLS.iter())
}

// True when the zazz tools are worth their place in every request: ZAZZLE_MCP_ZAZZ=1, or at least one script is registered; the zazz CLI works either way.
fn offers_zazz() -> bool {
    if let Ok(v) = std::env::var("ZAZZLE_MCP_ZAZZ") {
        return v == "1";
    }
    let meta = crate::zazz::registry::default_root().map(|r| r.join("meta"));
    meta.ok()
        .and_then(|m| std::fs::read_dir(m).ok())
        .is_some_and(|mut es| es.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json"))))
}

// Lists the tools with their schemas.
fn list_tools() -> Value {
    let zazz = offers_zazz();
    let tools: Vec<Value> = all_tools()
        .filter(|t| zazz || !t.name.starts_with("zazz_"))
        .filter(|t| agent_tool(t.name))
        .map(|t| {
            let mut schema = (t.schema)();
            if PINNABLE.contains(&t.name) {
                schema["properties"]["commit"] = json!({"type": "string", "description": "Answer only from repositories at this commit (sha or HEAD)"});
            }
            if takes_llm(t.name) {
                schema["properties"]["llm"] = json!({"type": "string", "description": "Have the local model process the output with this instruction first, returning only its answer (fewer tokens, may lose detail)"});
            }
            json!({"name": t.name, "description": t.description, "inputSchema": schema})
        })
        .collect();
    json!({"tools": tools})
}

// True when tools/list shows the tool: zazz scripts and the agent commands; the others still answer tools/call.
fn agent_tool(name: &str) -> bool {
    name.starts_with("zazz_")
        || name.strip_prefix("zazzle_").is_some_and(|c| crate::cmd::help::is_agent(&c.replace('_', "-")))
}

// The tools that read a repository and take `commit`.
const PINNABLE: &[&str] = &[
    "zazzle_find",
    "zazzle_read",
    "zazzle_ls",
    "zazzle_outline",
    "zazzle_refs",
    "zazzle_edit",
    "zazzle_overview",
    "zazzle_find_symbol",
    "zazzle_changed_since",
    "zazzle_git_log",
    "zazzle_git_blame",
];

// True when a tool's output may go through the local model first: tools that only read (not edits, nor the model's own tools).
fn takes_llm(name: &str) -> bool {
    !matches!(
        name,
        "zazzle_edit"
            | "zazzle_replace"
            | "zazzle_prompt"
            | "zazzle_brief"
            | "zazzle_investigate"
            | "zazzle_call"
    )
}

// Puts `--llm INSTRUCTION` right after the command name when a reading tool is given `llm`.
fn llm_args(tool: &str, mut argv: Vec<String>, args: &Value) -> Result<Vec<String>, String> {
    if let Some(i) = get_str(args, "llm")?.filter(|_| takes_llm(tool)) {
        argv.splice(1..1, ["--llm".to_string(), i]);
    }
    Ok(argv)
}

// Wraps text as an MCP tool result.
fn tool_result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

// Puts `--commit SHA` right after the command name when a repository tool is given `commit`.
fn pin_args(tool: &str, mut argv: Vec<String>, args: &Value) -> Result<Vec<String>, String> {
    if let Some(c) = get_str(args, "commit")?.filter(|_| PINNABLE.contains(&tool)) {
        argv.splice(1..1, ["--commit".to_string(), c]);
    }
    Ok(argv)
}

// The argument zazzle's hook adds to each tool call so a Claude Code session and each of its subagents keep their own dedupe state.
pub const SESSION_ARG: &str = "zazzle_session";

// Runs a tool call and returns its result, or a protocol error for an unknown tool.
fn call_tool(params: &Value, backend: &dyn Backend) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(tool) = all_tools().find(|t| t.name == name) else {
        return Err(format!("unknown tool '{name}'"));
    };
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    let argv =
        match (tool.map)(args).and_then(|a| pin_args(name, a, args)).and_then(|a| llm_args(name, a, args)) {
            Ok(a) => a,
            Err(e) => return Ok(tool_result(&format!("error: {e}"), true)),
        };
    let session = args.get(SESSION_ARG).and_then(Value::as_str);
    Ok(match backend.call(argv, session) {
        Ok((status, text)) => tool_result(&text, status != 0),
        Err(e) => tool_result(&format!("error: {e}"), true),
    })
}

// Handles one JSON-RPC line and returns the response, if one is due.
pub fn handle_line(line: &str, backend: &dyn Backend) -> Option<Value> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Some(rpc_error(Value::Null, -32700, "parse error"));
    };
    let method = msg.get("method").and_then(Value::as_str)?;
    let id = msg.get("id").cloned()?;
    let empty = json!({});
    let params = msg.get("params").unwrap_or(&empty);
    Some(match method {
        "initialize" => rpc_result(id, initialize(params)),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, list_tools()),
        "tools/call" => match call_tool(params, backend) {
            Ok(r) => rpc_result(id, r),
            Err(e) => rpc_error(id, -32602, &e),
        },
        _ => rpc_error(id, -32601, "method not found"),
    })
}

// The id of the roots/list request zazzle sends a client that offers roots.
const ROOTS_ID: &str = "zazzle-roots";

// Turns a roots/list result into folder paths: `file://` URIs, percent-decoded.
fn root_paths(result: &Value) -> Vec<String> {
    let decode = |s: &str| -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            match (
                b[i],
                b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()),
            ) {
                (b'%', Some(v)) => {
                    out.push(v);
                    i += 3;
                }
                (c, _) => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    result["roots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["uri"].as_str()?.strip_prefix("file://").map(decode))
        .collect()
}

// Serves newline-delimited JSON-RPC from the input until it closes; asks a client that offers roots for them, and again when they change.
pub fn serve(input: &mut dyn BufRead, output: &mut dyn Write, backend: &dyn Backend) -> io::Result<()> {
    serve_resumed(input, output, backend, None)
}

// Serves like serve, first answering a request an earlier program handed over and telling the client the tool list changed.
pub fn serve_resumed(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    backend: &dyn Backend,
    resumed: Option<(String, bool)>,
) -> io::Result<()> {
    let mut offers_roots = false;
    if let Some((line, roots)) = resumed {
        offers_roots = roots;
        writeln!(output, "{}", json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}))?;
        if roots {
            writeln!(output, "{}", json!({"jsonrpc": "2.0", "id": ROOTS_ID, "method": "roots/list"}))?;
        }
        if let Some(resp) = handle_line(&line, backend) {
            writeln!(output, "{resp}")?;
        }
        output.flush()?;
    }
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        if msg["method"].is_string() && !msg["id"].is_null() && msg["method"] != "initialize" {
            backend.upgrade(&line, offers_roots);
        }
        match msg["method"].as_str() {
            Some("initialize") => offers_roots = msg.pointer("/params/capabilities/roots").is_some(),
            None if msg["id"] == ROOTS_ID => {
                backend.set_roots(root_paths(&msg["result"]));
                continue;
            }
            _ => {}
        }
        let ask_roots = offers_roots
            && matches!(
                msg["method"].as_str(),
                Some("notifications/initialized" | "notifications/roots/list_changed")
            );
        if ask_roots {
            writeln!(output, "{}", json!({"jsonrpc": "2.0", "id": ROOTS_ID, "method": "roots/list"}))?;
            output.flush()?;
        }
        if let Some(resp) = handle_line(&line, backend) {
            writeln!(output, "{resp}")?;
            output.flush()?;
        }
    }
    Ok(())
}

// Reads six random bytes from the operating system.
fn random_bytes() -> io::Result<[u8; 6]> {
    use std::io::Read;
    let mut bytes = [0u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

// Derives six bytes from the clock and process id when no randomness is available.
fn fallback_bytes(nanos: u128, pid: u32) -> [u8; 6] {
    let mixed = (nanos ^ u128::from(pid)).to_le_bytes();
    [mixed[0], mixed[1], mixed[2], mixed[3], mixed[4], mixed[5]]
}

// Makes a short random session id for this MCP connection.
pub fn new_session_id() -> String {
    let bytes = random_bytes().unwrap_or_else(|_| {
        let t =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        fallback_bytes(t, std::process::id())
    });
    format!("mcp-{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // A fake backend that records calls and returns a fixed answer.
    struct Fake {
        calls: RefCell<Vec<Vec<String>>>,
        answer: Result<(u8, String), String>,
        roots: RefCell<Vec<String>>,
        sessions: RefCell<Vec<Option<String>>>,
    }

    impl Backend for Fake {
        // Records the call and replies.
        fn call(&self, args: Vec<String>, session: Option<&str>) -> Result<(u8, String), String> {
            self.calls.borrow_mut().push(args);
            self.sessions.borrow_mut().push(session.map(String::from));
            self.answer.clone()
        }

        // Records the roots.
        fn set_roots(&self, roots: Vec<String>) {
            *self.roots.borrow_mut() = roots;
        }
    }

    // Builds a fake that answers with text.
    fn fake(status: u8, text: &str) -> Fake {
        Fake {
            calls: RefCell::new(vec![]),
            roots: RefCell::new(vec![]),
            sessions: RefCell::new(vec![]),
            answer: Ok((status, text.into())),
        }
    }

    // Sends one request line and returns the response.
    fn ask(b: &Fake, req: Value) -> Value {
        handle_line(&req.to_string(), b).expect("response")
    }

    // Checks initialize negotiation for a known and an unknown version.
    #[test]
    fn test_initialize_versions() {
        let b = fake(0, "");
        let r = ask(
            &b,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        );
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "zazzle");
        let r = ask(
            &b,
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999"}}),
        );
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        let r = ask(&b, json!({"jsonrpc":"2.0","id":3,"method":"initialize"}));
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    }

    // Checks tools/list and both instruction texts name no command meant only for people.
    #[test]
    fn test_agent_surface_hides_human_commands() {
        let b = fake(0, "");
        let l = ask(&b, json!({"id":1,"method":"tools/list"}));
        let tools = l["result"]["tools"].to_string();
        let texts = [tools.as_str(), crate::setup::MCP_INSTRUCTIONS, crate::setup::INSTRUCTIONS];
        for human in [
            "overview",
            "changed_since",
            "git_log",
            "git_blame",
            "report_gap",
            "status",
            "languages",
            "setup",
            "trace",
            "proxy",
        ] {
            for (i, t) in texts.iter().enumerate() {
                assert!(
                    !t.contains(&format!("zazzle_{human}")) && !t.contains(&format!("zazzle {human}")),
                    "{i}: {human}"
                );
            }
        }
        assert!(tools.contains("zazzle_find_symbol") && tools.contains("zazzle_edit"));
    }

    // Checks diff and replace arguments become their command lines.
    #[test]
    fn test_diff_and_replace_args() {
        assert_eq!(
            diff_args(&json!({"range": "main...HEAD", "paths": ["src"], "names": true, "budget": 9}))
                .expect("diff"),
            vec!["diff", "main...HEAD", "src", "--names", "--budget", "9"]
        );
        assert_eq!(diff_args(&json!({})).expect("bare"), vec!["diff"]);
        let r = replace_args(&json!({"pattern": "a", "replacement": "b", "paths": ["p"], "glob": ["*.rs"], "dry_run": true, "fixed": true})).expect("replace");
        assert_eq!(r, vec!["replace", "a", "b", "p", "-g", "*.rs", "--dry-run", "--fixed"]);
        assert!(replace_args(&json!({"pattern": "a"})).is_err());
    }

    // Checks edit pairs become repeated --old/--new options.
    #[test]
    fn test_edit_pairs() {
        let a = edit_args(&json!({"path": "f", "expect": "s", "edits": [{"old": "a", "new": "b"}, {"old": "c", "new": "d"}]})).expect("args");
        assert_eq!(
            a,
            vec!["edit", "f", "--expect", "s", "--old", "a", "--new", "b", "--old", "c", "--new", "d"]
        );
        assert!(edit_args(&json!({"path": "f", "edits": [{"old": "a"}]})).is_err());
    }

    // Checks the hook's session argument reaches the backend and is not passed to the command.
    #[test]
    fn test_session_argument() {
        let b = fake(0, "ok");
        ask(
            &b,
            json!({"id":1,"method":"tools/call","params":{"name":"zazzle_ls","arguments":{"zazzle_session":"s1/a2"}}}),
        );
        ask(&b, json!({"id":2,"method":"tools/call","params":{"name":"zazzle_ls","arguments":{}}}));
        assert_eq!(*b.sessions.borrow(), vec![Some("s1/a2".to_string()), None]);
        assert!(!b.calls.borrow()[0].iter().any(|a| a.contains("s1/a2")));
    }

    // Checks ping, tools/list and unknown methods.
    #[test]
    fn test_ping_list_and_unknown() {
        let b = fake(0, "");
        assert_eq!(ask(&b, json!({"id":1,"method":"ping"}))["result"], json!({}));
        let l = ask(&b, json!({"id":2,"method":"tools/list"}));
        let names: Vec<&str> = l["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert!(
            names.contains(&"zazzle_find") && names.contains(&"zazzle_read") && names.contains(&"zazzle_ls")
        );
        assert!(names.contains(&"zazzle_read_again"));
        ask(
            &b,
            json!({"id":4,"method":"tools/call","params":{"name":"zazzle_read_again","arguments":{"id":"r:abcde"}}}),
        );
        assert_eq!(b.calls.borrow().last().expect("call"), &["read-again", "r:abcde"]);
        ask(
            &b,
            json!({"id":5,"method":"tools/call","params":{"name":"zazzle_read_again","arguments":{"id":"x","full":true,"budget":9,"lines":"2-4"}}}),
        );
        assert_eq!(
            b.calls.borrow().last().expect("call"),
            &["read-again", "x", "--full", "--budget", "9", "--lines", "2-4"]
        );
        assert_eq!(ask(&b, json!({"id":3,"method":"nope"}))["error"]["code"], -32601);
    }

    // Checks notifications and responses get no reply, and bad JSON gets a parse error.
    #[test]
    fn test_notifications_and_parse_errors() {
        let b = fake(0, "");
        assert!(handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &b).is_none());
        assert!(handle_line(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, &b).is_none());
        assert_eq!(handle_line("{", &b).expect("error")["error"]["code"], -32700);
    }

    // Checks a find call maps to the right command line and returns text.
    #[test]
    fn test_call_find_maps_arguments() {
        let b = fake(0, "result text");
        let r = ask(
            &b,
            json!({"id":1,"method":"tools/call","params":{"name":"zazzle_find","arguments":{
            "pattern":"-x", "paths":["src"], "glob":["*.rs"], "ignore_case":true, "fixed":true, "word":true,
            "count":false, "files_only":true, "no_ignore":true, "context":2, "max":10, "disable_session_deduping":true}}}),
        );
        assert_eq!(r["result"]["content"][0]["text"], "result text");
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(
            b.calls.borrow()[0],
            [
                "find",
                "-i",
                "-F",
                "-w",
                "-l",
                "-u",
                "--disable-session-deduping",
                "-C",
                "2",
                "-m",
                "10",
                "-g",
                "*.rs",
                "--",
                "-x",
                "src"
            ]
        );
    }

    // Checks read and ls argument mapping.
    #[test]
    fn test_call_read_and_ls_map_arguments() {
        let b = fake(0, "ok");
        ask(
            &b,
            json!({"id":1,"method":"tools/call","params":{"name":"zazzle_read","arguments":{
            "paths":["a:1-2"], "around":5, "ctx":1, "head":2, "tail":3, "budget":100, "anchors":true, "source":true, "full":true, "symbol":"A.b"}}}),
        );
        ask(
            &b,
            json!({"id":2,"method":"tools/call","params":{"name":"zazzle_ls","arguments":{"path":"src","depth":1,"max":5,"no_ignore":true}}}),
        );
        ask(&b, json!({"id":3,"method":"tools/call","params":{"name":"zazzle_ls"}}));
        ask(
            &b,
            json!({"id":4,"method":"tools/call","params":{"name":"zazzle_read","arguments":{"symbol":"Acme.A.b"}}}),
        );
        let calls = b.calls.borrow();
        assert_eq!(calls[3], ["read", "--symbol", "Acme.A.b", "--"]);
        assert_eq!(
            calls[0],
            [
                "read",
                "--anchors",
                "--source",
                "--full",
                "--around",
                "5",
                "--ctx",
                "1",
                "--head",
                "2",
                "--tail",
                "3",
                "--budget",
                "100",
                "--symbol",
                "A.b",
                "--",
                "a:1-2"
            ]
        );
        assert_eq!(calls[1], ["ls", "-u", "-d", "1", "-m", "5", "--", "src"]);
        assert_eq!(calls[2], ["ls"]);
    }

    // Checks argument validation errors come back as tool errors.
    #[test]
    fn test_call_argument_errors() {
        let b = fake(0, "ok");
        let text = |args: Value, name: &str| {
            let r = ask(&b, json!({"id":1,"method":"tools/call","params":{"name":name,"arguments":args}}));
            assert_eq!(r["result"]["isError"], true);
            r["result"]["content"][0]["text"].as_str().expect("text").to_string()
        };
        assert_eq!(text(json!({}), "zazzle_find"), "error: 'pattern' is required");
        assert_eq!(text(json!({"pattern": 5}), "zazzle_find"), "error: 'pattern' must be a string");
        assert_eq!(
            text(json!({"pattern":"x","paths":"src"}), "zazzle_find"),
            "error: 'paths' must be a list of strings"
        );
        assert_eq!(
            text(json!({"pattern":"x","paths":[1]}), "zazzle_find"),
            "error: 'paths' must be a list of strings"
        );
        assert_eq!(
            text(json!({"pattern":"x","fixed":"yes"}), "zazzle_find"),
            "error: 'fixed' must be true or false"
        );
        assert_eq!(
            text(json!({"pattern":"x","max":-1}), "zazzle_find"),
            "error: 'max' must be a non-negative integer"
        );
        assert_eq!(text(json!({}), "zazzle_read"), "error: 'paths' is required (or give symbol alone)");
        assert!(b.calls.borrow().is_empty());
    }

    // Checks nonzero status and backend failures are tool errors, and unknown tools protocol errors.
    #[test]
    fn test_call_status_and_backend_errors() {
        let b = fake(2, "error: bad");
        let r = ask(&b, json!({"id":1,"method":"tools/call","params":{"name":"zazzle_ls","arguments":{}}}));
        assert_eq!(
            (r["result"]["isError"].as_bool(), r["result"]["content"][0]["text"].as_str()),
            (Some(true), Some("error: bad"))
        );
        let down = Fake {
            calls: RefCell::new(vec![]),
            roots: RefCell::new(vec![]),
            sessions: RefCell::new(vec![]),
            answer: Err("cannot reach daemon: boom".into()),
        };
        let r =
            ask(&down, json!({"id":2,"method":"tools/call","params":{"name":"zazzle_ls","arguments":{}}}));
        assert_eq!(r["result"]["content"][0]["text"], "error: cannot reach daemon: boom");
        let r = ask(&b, json!({"id":3,"method":"tools/call","params":{"name":"zzz"}}));
        assert_eq!(r["error"]["code"], -32602);
        let r = ask(&b, json!({"id":4,"method":"tools/call","params":{}}));
        assert_eq!(r["error"]["message"], "unknown tool ''");
    }

    // Checks the serve loop reads lines, skips blanks and writes responses.
    #[test]
    fn test_serve_loop() {
        let b = fake(0, "");
        let input = "\n{\"id\":1,\"method\":\"ping\"}\n{\"method\":\"notifications/initialized\"}\n{\"id\":2,\"method\":\"ping\"}\n";
        let mut out = Vec::new();
        serve(&mut input.as_bytes(), &mut out, &b).expect("serve");
        let lines: Vec<&str> = std::str::from_utf8(&out).expect("utf8").lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"id\":1") && lines[1].contains("\"id\":2"));
    }

    // Checks a client offering roots is asked for them after initializing and when they change, and the answer is kept, decoded.
    #[test]
    fn test_serve_roots() {
        let b = fake(0, "");
        let input = [
            json!({"id": 1, "method": "initialize", "params": {"capabilities": {"roots": {"listChanged": true}}}}),
            json!({"method": "notifications/initialized"}),
            json!({"id": "zazzle-roots", "result": {"roots": [{"uri": "file:///w/my%20app"}, {"uri": "https://x"}, {"uri": "file:///w/%zz"}]}}),
            json!({"method": "notifications/roots/list_changed"}),
            json!({"id": 7, "result": {}}),
        ]
        .iter()
        .map(|v| v.to_string() + "\n")
        .collect::<String>();
        let mut out = Vec::new();
        serve(&mut input.as_bytes(), &mut out, &b).expect("serve");
        let text = String::from_utf8(out).expect("utf8");
        assert_eq!(text.matches("\"method\":\"roots/list\"").count(), 2, "{text}");
        assert_eq!(*b.roots.borrow(), ["/w/my app", "/w/%zz"]);
        let mut out = Vec::new();
        let pending = json!({"id": 9, "method": "tools/list"}).to_string();
        serve_resumed(&mut "".as_bytes(), &mut out, &fake(0, ""), Some((pending, true))).expect("resume");
        let text = String::from_utf8(out).expect("utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].contains("notifications/tools/list_changed") && lines[1].contains("roots/list"),
            "{text}"
        );
        assert!(lines[2].contains("\"id\":9") && lines[2].contains("zazzle_get_rules"), "{text}");
        let mut out = Vec::new();
        let note = json!({"method": "notifications/initialized"}).to_string();
        serve_resumed(&mut "".as_bytes(), &mut out, &fake(0, ""), Some((note, false))).expect("resume");
        assert_eq!(String::from_utf8(out).expect("utf8").lines().count(), 1);
        let plain = fake(0, "");
        let mut out = Vec::new();
        let input = "{\"id\":1,\"method\":\"initialize\",\"params\":{}}\n{\"method\":\"notifications/initialized\"}\n";
        serve(&mut input.as_bytes(), &mut out, &plain).expect("serve");
        assert!(!String::from_utf8(out).expect("utf8").contains("roots/list"));
        let be = DaemonBackend {
            sock: "/nonexistent".into(),
            cwd: "/".into(),
            session: "s".into(),
            roots: Default::default(),
        };
        be.set_roots(vec!["/w".into()]);
        assert_eq!(*be.roots.borrow(), ["/w"]);
        assert_eq!(
            crate::proto::split_session(&crate::proto::join_session("s", &["/w".into(), "/v".into()])),
            ("s".to_string(), vec![PathBuf::from("/w"), PathBuf::from("/v")])
        );
    }

    // Checks repository tools take commit, passed as --commit after the command, and others do not offer it.
    #[test]
    fn test_commit_argument() {
        let b = fake(0, "ok");
        ask(
            &b,
            json!({"id":5,"method":"tools/call","params":{"name":"zazzle_find","arguments":{"pattern":"x","commit":"abc123"}}}),
        );
        assert_eq!(b.calls.borrow()[0][..3], ["find", "--commit", "abc123"]);
        let bad = ask(
            &b,
            json!({"id":6,"method":"tools/call","params":{"name":"zazzle_find","arguments":{"pattern":"x","commit":1}}}),
        );
        assert_eq!(bad["result"]["content"][0]["text"], "error: 'commit' must be a string");
        let tools = list_tools();
        let schema = |n: &str| {
            tools["tools"].as_array().expect("tools").iter().find(|t| t["name"] == n).expect("tool")
                ["inputSchema"]
                .clone()
        };
        assert!(
            schema("zazzle_read")["properties"]["commit"].is_object()
                && schema("zazzle_read_again")["properties"]["commit"].is_null()
        );
    }

    // Checks reading tools take llm, passed as --llm after the command, edits and the model's own tools do not, and zazzle_prompt maps its arguments.
    #[test]
    fn test_llm_argument() {
        let b = fake(0, "ok");
        ask(
            &b,
            json!({"id":1,"method":"tools/call","params":{"name":"zazzle_find","arguments":{"pattern":"x","llm":"Only file names"}}}),
        );
        ask(
            &b,
            json!({"id":2,"method":"tools/call","params":{"name":"zazzle_prompt","arguments":{"text":"Hi?","effort":"low","llm":"x"}}}),
        );
        ask(
            &b,
            json!({"id":3,"method":"tools/call","params":{"name":"zazzle_brief","arguments":{"intent":"fix it"}}}),
        );
        let calls = b.calls.borrow();
        assert_eq!(calls[0][..3], ["find", "--llm", "Only file names"]);
        assert_eq!(calls[1], ["prompt", "--effort", "low", "Hi?"]);
        assert_eq!(calls[2], ["brief", "fix it"]);
        let tools = list_tools();
        let props = |n: &str| {
            tools["tools"].as_array().expect("tools").iter().find(|t| t["name"] == n).expect(n)["inputSchema"]
                ["properties"]
                .clone()
        };
        assert!(props("zazzle_read")["llm"].is_object() && props("zazzle_edit")["llm"].is_null());
        assert!(props("zazzle_brief")["intent"].is_object() && props("zazzle_prompt")["llm"].is_null());
    }

    // Checks write failures surface as errors.
    #[test]
    fn test_serve_write_error() {
        struct Broken;
        impl Write for Broken {
            // Always fails.
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("closed"))
            }
            // Always succeeds.
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let b = fake(0, "");
        assert!(serve(&mut "{\"id\":1,\"method\":\"ping\"}\n".as_bytes(), &mut Broken, &b).is_err());
        let roots = "{\"id\":1,\"method\":\"initialize\",\"params\":{\"capabilities\":{\"roots\":{}}}}\n{\"method\":\"notifications/initialized\"}\n";
        assert!(serve(&mut roots.as_bytes(), &mut Broken, &b).is_err());
    }

    // Checks session ids look like mcp- plus twelve hex digits and differ.
    #[test]
    fn test_session_ids() {
        let (a, b) = (new_session_id(), new_session_id());
        assert!(a.starts_with("mcp-") && a.len() == 16 && a != b);
    }

    // Checks the daemon backend reports an unreachable daemon clearly.
    #[test]
    fn test_daemon_backend_errors() {
        let dir = crate::testutil::TempDir::new();
        let file = dir.write("f", b"x");
        let be =
            DaemonBackend { sock: file, cwd: "/".into(), session: "s".into(), roots: Default::default() };
        assert!(be.call(vec!["version".into()], None).unwrap_err().starts_with("cannot reach daemon: "));
    }

    // Checks the daemon backend talks to a live server.
    #[test]
    fn test_daemon_backend_live() {
        let srv = crate::server::tests::Running::start(std::time::Duration::from_secs(30));
        let be = DaemonBackend {
            sock: srv.sock.clone(),
            cwd: "/".into(),
            session: "s".into(),
            roots: Default::default(),
        };
        let (status, text) = be.call(vec!["version".into()], None).expect("call");
        assert_eq!(status, 0);
        assert!(text.starts_with("zazzle "));
        srv.finish().expect("finish");
    }

    // Checks the random and fallback byte sources.
    #[test]
    fn test_random_and_fallback_bytes() {
        assert_ne!(random_bytes().expect("urandom"), random_bytes().expect("urandom"));
        assert_eq!(fallback_bytes(0x0102_0304_0506, 0), [6, 5, 4, 3, 2, 1]);
        assert_eq!(fallback_bytes(1, 3)[0], 2);
    }

    // Checks outline, refs and edit argument mapping.
    #[test]
    fn test_call_outline_refs_edit_map_arguments() {
        let b = fake(0, "ok");
        ask(
            &b,
            json!({"id":1,"method":"tools/call","params":{"name":"zazzle_outline","arguments":{"paths":["src"],"depth":1,"max":9,"disable_session_deduping":true}}}),
        );
        ask(
            &b,
            json!({"id":2,"method":"tools/call","params":{"name":"zazzle_refs","arguments":{"symbol":"-x","paths":["a"],"glob":["*.rs"],"max":3,"no_ignore":true,"disable_session_deduping":true}}}),
        );
        ask(
            &b,
            json!({"id":3,"method":"tools/call","params":{"name":"zazzle_edit","arguments":{
            "path":"a.rs","old":"x","new":"y","all":true,"create":true,"after":2,"lines":"1-2","at":"1#a-2#b","expect":"abcd"}}}),
        );
        ask(
            &b,
            json!({"id":4,"method":"tools/call","params":{"name":"zazzle_edit","arguments":{"path":"b.rs","new":"z"}}}),
        );
        let calls = b.calls.borrow();
        assert_eq!(calls[3], ["edit", "b.rs", "--new", "z"]);
        assert_eq!(calls[0], ["outline", "--disable-session-deduping", "-d", "1", "-m", "9", "--", "src"]);
        assert_eq!(
            calls[1],
            ["refs", "-u", "--disable-session-deduping", "-m", "3", "-g", "*.rs", "--", "-x", "a"]
        );
        assert_eq!(
            calls[2],
            [
                "edit", "a.rs", "--all", "--create", "--after", "2", "--old", "x", "--new", "y", "--lines",
                "1-2", "--at", "1#a-2#b", "--expect", "abcd"
            ]
        );
    }

    // Checks an agent's max, budget and limit are lowered to the ceilings, smaller ones pass through, and code_only maps to its flag.
    #[test]
    fn test_ceilings_and_code_only() {
        let b = fake(0, "ok");
        for (name, args) in [
            ("zazzle_find", json!({"pattern": "x", "max": 100, "code_only": true})),
            ("zazzle_refs", json!({"symbol": "x", "max": 100, "code_only": true})),
            ("zazzle_ls", json!({"max": 500})),
            ("zazzle_read", json!({"paths": ["a"], "budget": 9000})),
            ("zazzle_read_again", json!({"id": "r", "full": true, "budget": 9000})),
            ("zazzle_find_symbol", json!({"name": "x", "limit": 50})),
            ("zazzle_find", json!({"pattern": "x", "max": 5})),
        ] {
            ask(&b, json!({"id":1,"method":"tools/call","params":{"name":name,"arguments":args}}));
        }
        let calls = b.calls.borrow();
        assert_eq!(calls[0], ["find", "--code-only", "-m", "40", "--", "x"]);
        assert_eq!(calls[1], ["refs", "--code-only", "-m", "40", "--", "x"]);
        assert_eq!(calls[2], ["ls", "-m", "60"]);
        assert_eq!(calls[3], ["read", "--budget", "9000", "--", "a"]);
        assert_eq!(calls[4], ["read-again", "r", "--full", "--budget", "2500"]);
        assert_eq!(calls[5], ["find-symbol", "--limit", "20", "--", "x"]);
        assert_eq!(calls[6], ["find", "-m", "5", "--", "x"]);
    }

    // Checks argument validation for the new tools.
    #[test]
    fn test_new_tool_argument_errors() {
        let b = fake(0, "ok");
        let text = |name: &str, args: Value| {
            let r = ask(&b, json!({"id":1,"method":"tools/call","params":{"name":name,"arguments":args}}));
            r["result"]["content"][0]["text"].as_str().expect("text").to_string()
        };
        assert_eq!(text("zazzle_outline", json!({})), "error: 'paths' is required");
        assert_eq!(text("zazzle_refs", json!({})), "error: 'symbol' is required");
        assert_eq!(text("zazzle_read_again", json!({})), "error: 'id' is required");
        assert_eq!(text("zazzle_edit", json!({"new": "x"})), "error: 'path' is required");
        assert_eq!(text("zazzle_edit", json!({"path": "a", "old": 5})), "error: 'old' must be a string");
        assert!(b.calls.borrow().is_empty());
    }

    // Checks every tool has a schema object with its required list.
    #[test]
    fn test_all_tool_schemas() {
        let _g = crate::testutil::env_lock();
        let saved = (std::env::var_os("ZAZZLE_MCP_ZAZZ"), std::env::var_os("ZAZZ_STORE"));
        std::env::set_var("ZAZZLE_MCP_ZAZZ", "1");
        let l = list_tools();
        for t in l["tools"].as_array().expect("tools") {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["inputSchema"]["required"].is_array());
        }
        assert_eq!(l["tools"].as_array().expect("tools").len(), 23);
        assert!(all_tools().all(|t| (t.schema)()["type"] == "object"));
        std::env::set_var("ZAZZLE_MCP_ZAZZ", "0");
        assert_eq!(list_tools()["tools"].as_array().expect("tools").len(), 16);
        std::env::remove_var("ZAZZLE_MCP_ZAZZ");
        let store = crate::testutil::TempDir::new();
        std::env::set_var("ZAZZ_STORE", store.path());
        assert!(!offers_zazz(), "no scripts registered");
        store.write("meta/abc.json", b"{}");
        assert!(offers_zazz(), "a registered script brings the zazz tools");
        for (k, v) in [("ZAZZLE_MCP_ZAZZ", saved.0), ("ZAZZ_STORE", saved.1)] {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    // Checks zazzle_stat and zazzle_call map to their command lines and reject missing arguments.
    #[test]
    fn test_stat_and_call_args() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            stat_args(&json!({"paths": ["a", "-b"], "hash": true})),
            Ok(v(&["stat", "--hash", "--", "a", "-b"]))
        );
        assert_eq!(stat_args(&json!({})).unwrap_err(), "'paths' is required");
        let one = json!({"op": "stat", "path": "a"});
        assert_eq!(call_args(&json!({"requests": one})), Ok(v(&["call", &one.to_string()])));
        assert_eq!(call_args(&json!({"requests": [one]})), Ok(v(&["call", &json!([one]).to_string()])));
        assert_eq!(call_args(&json!({"requests": "{}"})), Ok(v(&["call", "{}"])));
        assert!(call_args(&json!({"requests": 1})).unwrap_err().contains("'requests' is required"));
        assert!(!takes_llm("zazzle_call") && takes_llm("zazzle_stat"));
    }

    // Checks zazzle_prompt's arguments with and without an effort, and a wrong type for it.
    #[test]
    fn test_prompt_args() {
        assert_eq!(prompt_args(&json!({"text": "hi"})).expect("plain"), ["prompt", "hi"]);
        assert_eq!(
            prompt_args(&json!({"text": "hi", "effort": "high"})).expect("effort"),
            ["prompt", "--effort", "high", "hi"]
        );
        assert!(prompt_args(&json!({"text": "hi", "effort": 3})).is_err());
        assert!(prompt_args(&json!({})).is_err());
    }
}
