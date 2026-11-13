// MCP stdio front end: exposes the zazzle commands as tools and forwards them to the daemon.
use crate::client;
use crate::proto::Request;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

// Anything that can run a command line and return its status and text.
pub trait Backend {
    // Runs the arguments as one zazzle command.
    fn call(&self, args: Vec<String>) -> Result<(u8, String), String>;
}

// The real backend: a unix-socket client that starts the daemon on demand.
pub struct DaemonBackend {
    pub sock: PathBuf,
    pub cwd: String,
    pub session: String,
}

impl Backend for DaemonBackend {
    // Sends the command to zazzled in relay mode.
    fn call(&self, args: Vec<String>) -> Result<(u8, String), String> {
        let req = Request { cwd: self.cwd.clone(), session: self.session.clone(), args };
        client::call_autostart(&self.sock, &req, &client::spawn_daemon)
            .map_err(|e| format!("cannot reach zazzled: {e}"))
    }
}

// One exposed tool: how it is described and how its arguments become a command line.
struct Tool {
    name: &'static str,
    description: &'static str,
    schema: fn() -> Value,
    map: fn(&Value) -> Result<Vec<String>, String>,
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
        description: "Read files, line ranges (PATH or PATH:A-B) or one named symbol with its docs, as numbered lines; long output is elided to a token budget.",
        schema: read_schema,
        map: read_args,
    },
    Tool { name: "zazzle_ls", description: "Compact directory tree with file counts and line counts.", schema: ls_schema, map: ls_args },
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
        name: "zazzle_edit",
        description: "Edit one file safely: replace unique text, replace lines (needs expect sha or anchors from zazzle_read), insert after a line, or create a file. Writes are confined to the working directory.",
        schema: edit_schema,
        map: edit_args,
    },
];

// Builds a JSON schema object from properties and required names.
fn object(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
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
            "context": {"type": "integer"}, "max": {"type": "integer", "description": "Maximum matches shown (default 50)"},
            "disable_session_deduping": {"type": "boolean"}
        }),
        &["pattern"],
    )
}

// Schema for zazzle_read.
fn read_schema() -> Value {
    object(
        json!({
            "paths": {"type": "array", "items": {"type": "string"}, "description": "PATH or PATH:A-B (A-, A also accepted)"},
            "symbol": {"type": "string", "description": "Read one symbol by name (Type.method or a::b; #N picks among duplicates) with its doc comments and attributes"},
            "around": {"type": "integer"}, "ctx": {"type": "integer"}, "head": {"type": "integer"}, "tail": {"type": "integer"},
            "budget": {"type": "integer", "description": "Token budget (default 3000)"},
            "anchors": {"type": "boolean"}, "full": {"type": "boolean", "description": "No budget"},
            "disable_session_deduping": {"type": "boolean"}
        }),
        &["paths"],
    )
}

// Schema for zazzle_ls.
fn ls_schema() -> Value {
    object(
        json!({
            "path": {"type": "string"}, "depth": {"type": "integer"}, "max": {"type": "integer"},
            "no_ignore": {"type": "boolean"}, "disable_session_deduping": {"type": "boolean"}
        }),
        &[],
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
            "symbol": {"type": "string"}, "paths": {"type": "array", "items": {"type": "string"}},
            "glob": {"type": "array", "items": {"type": "string"}}, "max": {"type": "integer"},
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
            "expect": {"type": "string", "description": "The sha from zazzle_read's header"}
        }),
        &["path", "new"],
    )
}

// Reads an optional string argument.
fn get_str(v: &Value, key: &str) -> Result<Option<String>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("'{key}' must be a string")),
    }
}

// Reads a required string argument.
fn need_str(v: &Value, key: &str) -> Result<String, String> {
    get_str(v, key)?.ok_or_else(|| format!("'{key}' is required"))
}

// Reads an optional list of strings.
fn get_list(v: &Value, key: &str) -> Result<Vec<String>, String> {
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
fn push_flag(out: &mut Vec<String>, v: &Value, key: &str, flag: &str) -> Result<(), String> {
    match v.get(key) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(()),
        Some(Value::Bool(true)) => {
            out.push(flag.into());
            Ok(())
        }
        Some(_) => Err(format!("'{key}' must be true or false")),
    }
}

// Pushes a numeric option when the argument is present.
fn push_num(out: &mut Vec<String>, v: &Value, key: &str, flag: &str) -> Result<(), String> {
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
        ("disable_session_deduping", "--disable-session-deduping"),
    ] {
        push_flag(&mut a, v, k, f)?;
    }
    push_num(&mut a, v, "context", "-C")?;
    push_num(&mut a, v, "max", "-m")?;
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
        ("full", "--full"),
        ("disable_session_deduping", "--disable-session-deduping"),
    ] {
        push_flag(&mut a, v, k, f)?;
    }
    for (k, f) in [
        ("around", "--around"),
        ("ctx", "--ctx"),
        ("head", "--head"),
        ("tail", "--tail"),
        ("budget", "--budget"),
    ] {
        push_num(&mut a, v, k, f)?;
    }
    if let Some(sym) = get_str(v, "symbol")? {
        a.push("--symbol".into());
        a.push(sym);
    }
    let paths = get_list(v, "paths")?;
    if paths.is_empty() {
        return Err("'paths' is required".into());
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
    push_num(&mut a, v, "max", "-m")?;
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

// Maps zazzle_refs arguments to a command line.
fn refs_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["refs".to_string()];
    push_flag(&mut a, v, "no_ignore", "-u")?;
    push_flag(&mut a, v, "disable_session_deduping", "--disable-session-deduping")?;
    push_num(&mut a, v, "max", "-m")?;
    for g in get_list(v, "glob")? {
        a.push("-g".into());
        a.push(g);
    }
    a.push("--".into());
    a.push(need_str(v, "symbol")?);
    a.extend(get_list(v, "paths")?);
    Ok(a)
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

// Answers initialize by agreeing a protocol version.
fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "zazzle", "version": crate::cmd::VERSION}
    })
}

// Lists the tools with their schemas.
fn list_tools() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": (t.schema)()}))
        .collect();
    json!({"tools": tools})
}

// Wraps text as an MCP tool result.
fn tool_result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

// Runs a tool call and returns its result, or a protocol error for an unknown tool.
fn call_tool(params: &Value, backend: &dyn Backend) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let Some(tool) = TOOLS.iter().find(|t| t.name == name) else {
        return Err(format!("unknown tool '{name}'"));
    };
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    let argv = match (tool.map)(args) {
        Ok(a) => a,
        Err(e) => return Ok(tool_result(&format!("error: {e}"), true)),
    };
    Ok(match backend.call(argv) {
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

// Serves newline-delimited JSON-RPC from the input until it closes.
pub fn serve(input: &mut dyn BufRead, output: &mut dyn Write, backend: &dyn Backend) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
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
    }

    impl Backend for Fake {
        // Records the call and replies.
        fn call(&self, args: Vec<String>) -> Result<(u8, String), String> {
            self.calls.borrow_mut().push(args);
            self.answer.clone()
        }
    }

    // Builds a fake that answers with text.
    fn fake(status: u8, text: &str) -> Fake {
        Fake { calls: RefCell::new(vec![]), answer: Ok((status, text.into())) }
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
            "paths":["a:1-2"], "around":5, "ctx":1, "head":2, "tail":3, "budget":100, "anchors":true, "full":true, "symbol":"A.b"}}}),
        );
        ask(
            &b,
            json!({"id":2,"method":"tools/call","params":{"name":"zazzle_ls","arguments":{"path":"src","depth":1,"max":5,"no_ignore":true}}}),
        );
        ask(&b, json!({"id":3,"method":"tools/call","params":{"name":"zazzle_ls"}}));
        let calls = b.calls.borrow();
        assert_eq!(
            calls[0],
            [
                "read",
                "--anchors",
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
        assert_eq!(text(json!({}), "zazzle_read"), "error: 'paths' is required");
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
        let down = Fake { calls: RefCell::new(vec![]), answer: Err("cannot reach zazzled: boom".into()) };
        let r =
            ask(&down, json!({"id":2,"method":"tools/call","params":{"name":"zazzle_ls","arguments":{}}}));
        assert_eq!(r["result"]["content"][0]["text"], "error: cannot reach zazzled: boom");
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
        let be = DaemonBackend { sock: file, cwd: "/".into(), session: "s".into() };
        assert!(be.call(vec!["version".into()]).unwrap_err().starts_with("cannot reach zazzled: "));
    }

    // Checks the daemon backend talks to a live server.
    #[test]
    fn test_daemon_backend_live() {
        let srv = crate::server::tests::Running::start(std::time::Duration::from_secs(30));
        let be = DaemonBackend { sock: srv.sock.clone(), cwd: "/".into(), session: "s".into() };
        let (status, text) = be.call(vec!["version".into()]).expect("call");
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
        assert_eq!(text("zazzle_edit", json!({"new": "x"})), "error: 'path' is required");
        assert_eq!(text("zazzle_edit", json!({"path": "a", "old": 5})), "error: 'old' must be a string");
        assert!(b.calls.borrow().is_empty());
    }

    // Checks every tool has a schema object with its required list.
    #[test]
    fn test_all_tool_schemas() {
        let l = list_tools();
        for t in l["tools"].as_array().expect("tools") {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["inputSchema"]["required"].is_array());
        }
        assert_eq!(l["tools"].as_array().expect("tools").len(), 6);
    }
}
