// The zazz tools for MCP clients: test, add, call, describe, list, pack and new, mapped to zazz command lines.
use crate::mcp::{get_str, need_str, object, push_flag, Tool};
use serde_json::{json, Value};

pub(crate) const TOOLS: &[Tool] = &[
    Tool {
        name: "zazz_new",
        description: "Create a zazz script directory (Rust, std + the zazz SDK; tests use zazz::test) to edit, test and pack.",
        schema: new_schema,
        map: new_args,
    },
    Tool {
        name: "zazz_test",
        description: "Build a zazz script directory or .zazz file to WASM and run its tests; nothing is registered. Returns compile diagnostics or a test report.",
        schema: test_schema,
        map: test_args,
    },
    Tool {
        name: "zazz_pack",
        description: "Pack a zazz script directory into a deterministic .zazz file; returns its path and id.",
        schema: pack_schema,
        map: pack_args,
    },
    Tool {
        name: "zazz_add",
        description: "Register a .zazz file: build, test once and keep it; returns the script id (b3:...) used by zazz_call.",
        schema: add_schema,
        map: add_args,
    },
    Tool {
        name: "zazz_call",
        description: "Run a registered zazz script in its sandbox: json-command with `input`, or one OpenAPI `operation` with params and body. Returns the script's JSON output.",
        schema: call_schema,
        map: call_args,
    },
    Tool {
        name: "zazz_describe",
        description: "What a registered zazz script is, how it runs, what it may touch and which operations it offers.",
        schema: id_schema,
        map: describe_args,
    },
    Tool { name: "zazz_ls", description: "Registered zazz scripts, newest first.", schema: empty_schema, map: ls_args },
];

// Schema for zazz_new.
fn new_schema() -> Value {
    object(
        json!({"name": {"type": "string", "description": "Script name: lowercase letters, digits and '-'"}}),
        &["name"],
    )
}

// Schema for zazz_test.
fn test_schema() -> Value {
    object(
        json!({"path": {"type": "string", "description": "Script directory or .zazz file"}, "update": {"type": "boolean", "description": "Rewrite tests/snapshots"}}),
        &["path"],
    )
}

// Schema for zazz_pack.
fn pack_schema() -> Value {
    object(
        json!({"dir": {"type": "string"}, "output": {"type": "string", "description": "Default: DIR.zazz beside DIR"}}),
        &["dir"],
    )
}

// Schema for zazz_add.
fn add_schema() -> Value {
    object(json!({"file": {"type": "string", "description": "Path of the .zazz file"}}), &["file"])
}

// Schema for zazz_call.
fn call_schema() -> Value {
    object(
        json!({
            "id": {"type": "string", "description": "Script id (b3:...), a prefix of 12+ hex digits, or an installed name"},
            "input": {"description": "JSON input for json-command"},
            "operation": {"type": "string", "description": "An operationId for service-call instead of json-command"},
            "params": {"type": "object"},
            "body": {}
        }),
        &["id"],
    )
}

// Schema for tools that take only a script id.
fn id_schema() -> Value {
    object(json!({"id": {"type": "string"}}), &["id"])
}

// Schema for tools without arguments.
fn empty_schema() -> Value {
    object(json!({}), &[])
}

// Maps zazz_new arguments.
fn new_args(v: &Value) -> Result<Vec<String>, String> {
    Ok(vec!["zazz".into(), "new".into(), need_str(v, "name")?])
}

// Maps zazz_test arguments.
fn test_args(v: &Value) -> Result<Vec<String>, String> {
    let mut out = vec!["zazz".into(), "test".into(), need_str(v, "path")?];
    push_flag(&mut out, v, "update", "--update")?;
    Ok(out)
}

// Maps zazz_pack arguments.
fn pack_args(v: &Value) -> Result<Vec<String>, String> {
    let mut out = vec!["zazz".into(), "pack".into(), need_str(v, "dir")?];
    if let Some(o) = get_str(v, "output")? {
        out.extend(["-o".into(), o]);
    }
    Ok(out)
}

// Maps zazz_add arguments.
fn add_args(v: &Value) -> Result<Vec<String>, String> {
    Ok(vec!["zazz".into(), "add".into(), need_str(v, "file")?])
}

// Maps zazz_call arguments: json-command by default, service-call when an operation is named.
fn call_args(v: &Value) -> Result<Vec<String>, String> {
    let mut out = vec!["zazz".into(), "call".into(), need_str(v, "id")?];
    match get_str(v, "operation")? {
        Some(op) => {
            out.push(op);
            if let Some(p) = v.get("params").filter(|p| !p.is_null()) {
                out.extend(["--params".into(), p.to_string()]);
            }
            if let Some(b) = v.get("body").filter(|b| !b.is_null()) {
                out.extend(["--body".into(), b.to_string()]);
            }
        }
        None => out.push(v.get("input").cloned().unwrap_or(Value::Null).to_string()),
    }
    Ok(out)
}

// Maps zazz_describe arguments.
fn describe_args(v: &Value) -> Result<Vec<String>, String> {
    Ok(vec!["zazz".into(), "describe".into(), need_str(v, "id")?])
}

// Maps zazz_ls arguments.
fn ls_args(_: &Value) -> Result<Vec<String>, String> {
    Ok(vec!["zazz".into(), "ls".into()])
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks every tool maps its arguments to the right command line.
    #[test]
    fn test_mappings() {
        let s = |v: Vec<String>| v.join(" ");
        assert_eq!(s(new_args(&json!({"name": "x"})).expect("new")), "zazz new x");
        assert_eq!(
            s(test_args(&json!({"path": "d", "update": true})).expect("test")),
            "zazz test d --update"
        );
        assert_eq!(
            s(pack_args(&json!({"dir": "d", "output": "o.zazz"})).expect("pack")),
            "zazz pack d -o o.zazz"
        );
        assert_eq!(s(pack_args(&json!({"dir": "d"})).expect("pack")), "zazz pack d");
        assert_eq!(s(add_args(&json!({"file": "f"})).expect("add")), "zazz add f");
        assert_eq!(
            s(call_args(&json!({"id": "i", "input": {"a": 1}})).expect("call")),
            r#"zazz call i {"a":1}"#
        );
        assert_eq!(s(call_args(&json!({"id": "i"})).expect("call")), "zazz call i null");
        let svc =
            call_args(&json!({"id": "i", "operation": "op", "params": {"n": 1}, "body": [1]})).expect("svc");
        assert_eq!(s(svc), r#"zazz call i op --params {"n":1} --body [1]"#);
        assert_eq!(s(call_args(&json!({"id": "i", "operation": "op"})).expect("svc")), "zazz call i op");
        assert_eq!(s(describe_args(&json!({"id": "i"})).expect("describe")), "zazz describe i");
        assert_eq!(s(ls_args(&json!({})).expect("ls")), "zazz ls");
        assert!(call_args(&json!({})).is_err() && new_args(&json!({})).is_err());
        for t in TOOLS {
            assert_eq!((t.schema)()["type"], "object", "{}", t.name);
        }
    }
}
