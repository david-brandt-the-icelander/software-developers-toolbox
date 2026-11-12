// A minimal Model Context Protocol server over stdio that exposes every operation as a tool.
use crate::engine::{all_ok, Engine};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const INSTRUCTIONS: &str = "Prefer these tools over shell cat/grep/sed/find. Replies carry an etag; \
repeating a read or search returns {unchanged:true, ref} when nothing changed, and a changed file read \
returns only hunks (apply in descending `at` order). Pass fresh:true to force full content. \
Output is capped: follow next_ranges (read), next_cursor (list) or narrow the query.";

// Return the tool list with a JSON schema for each operation.
fn tools() -> Value {
    let paths = json!({"type": "array", "items": {"type": "string"}});
    let tool = |name: &str, desc: &str, props: Value, required: Value| {
        json!({"name": name, "description": desc,
               "inputSchema": {"type": "object", "properties": props, "required": required,
                               "additionalProperties": false}})
    };
    json!([
        tool(
            "afs_read",
            "Read a file: ranges [[start,end]] (1-based), capped; session-aware (unchanged/diff).",
            json!({"path": {"type": "string"},
                   "ranges": {"type": "array", "items": {"type": "array", "items": {"type": "integer"}, "minItems": 2, "maxItems": 2}},
                   "max_lines": {"type": "integer"}, "max_bytes": {"type": "integer"},
                   "if_none_match": {"type": "string"}, "fresh": {"type": "boolean"}}),
            json!(["path"])
        ),
        tool(
            "afs_search",
            "Regex/literal search grouped by file; honours .gitignore; cached.",
            json!({"pattern": {"type": "string"}, "literal": {"type": "boolean"}, "ignore_case": {"type": "boolean"},
                   "paths": paths, "glob": paths, "context": {"type": "integer"}, "max_matches": {"type": "integer"},
                   "files_only": {"type": "boolean"}, "fields": paths, "hidden": {"type": "boolean"},
                   "no_ignore": {"type": "boolean"}, "fresh": {"type": "boolean"}}),
            json!(["pattern"])
        ),
        tool(
            "afs_list",
            "List files (depth 1 unless glob or depth is given); honours .gitignore.",
            json!({"path": {"type": "string"}, "glob": paths, "depth": {"type": "integer"}, "hidden": {"type": "boolean"},
                   "no_ignore": {"type": "boolean"}, "long": {"type": "boolean"}, "max_entries": {"type": "integer"},
                   "cursor": {"type": "integer"}, "fresh": {"type": "boolean"}}),
            json!([])
        ),
        tool(
            "afs_stat",
            "Metadata for one path; hash:true adds the content etag.",
            json!({"path": {"type": "string"}, "hash": {"type": "boolean"}}),
            json!(["path"])
        ),
        tool(
            "afs_outline",
            "Symbols ([start_line, end_line, kind, name]) per source file; read just those ranges.",
            json!({"paths": paths, "max_symbols": {"type": "integer"}, "fresh": {"type": "boolean"}}),
            json!([])
        ),
        tool(
            "afs_diff_since",
            "Line hunks between a previously received etag and the file now.",
            json!({"path": {"type": "string"}, "etag": {"type": "string"}}),
            json!(["path", "etag"])
        ),
    ])
}

// Build a JSON-RPC error reply.
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

// Answer one JSON-RPC message, returning None for notifications.
fn handle(engine: &mut Engine, msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let params = &msg["params"];
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "afs", "version": env!("CARGO_PKG_VERSION")},
            "instructions": INSTRUCTIONS}),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call_tool(engine, params),
        other => return Some(rpc_error(id, -32601, &format!("method not found: {other}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

// Run a tools/call request and wrap the engine reply as MCP text content.
fn call_tool(engine: &mut Engine, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let Some(op) = name.strip_prefix("afs_") else {
        return json!({"content": [{"type": "text", "text": format!("unknown tool {name}")}], "isError": true});
    };
    let mut args = params["arguments"].clone();
    if args.is_null() {
        args = json!({});
    }
    args["op"] = json!(op);
    let reply = engine.call(args);
    json!({"content": [{"type": "text", "text": reply.to_string()}], "isError": !all_ok(&reply)})
}

// Serve newline-delimited JSON-RPC on stdin and stdout until stdin closes.
pub fn serve() -> std::io::Result<()> {
    let mut engine = Engine::new();
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&mut engine, &msg),
            Err(e) => Some(rpc_error(Value::Null, -32700, &e.to_string())),
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Initialize, list tools and call a tool through the JSON-RPC handler.
    #[test]
    fn test_handle_lifecycle() {
        let mut e = Engine::new();
        let init = handle(&mut e, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "afs");
        assert!(handle(&mut e, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let list = handle(&mut e, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 6);
        let call = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                          "params": {"name": "afs_stat", "arguments": {"path": "/"}}});
        let reply = handle(&mut e, &call).unwrap();
        assert_eq!(reply["result"]["isError"], false);
        assert!(reply["result"]["content"][0]["text"].as_str().unwrap().contains("\"kind\":\"dir\""));
    }

    // Unknown methods and tools are reported as errors.
    #[test]
    fn test_handle_errors() {
        let mut e = Engine::new();
        let bad = handle(&mut e, &json!({"jsonrpc": "2.0", "id": 9, "method": "nope"})).unwrap();
        assert_eq!(bad["error"]["code"], -32601);
        let tool = call_tool(&mut e, &json!({"name": "other"}));
        assert_eq!(tool["isError"], true);
        let failed = call_tool(&mut e, &json!({"name": "afs_read", "arguments": {"path": "/no/such"}}));
        assert_eq!(failed["isError"], true);
    }
}
