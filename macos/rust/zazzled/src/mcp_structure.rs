// MCP tools for the structure commands answered by zazzle-csharp-indexer: descriptions, schemas and argument mapping.
use crate::mcp::{get_str, need_str, object, push_flag, push_num, Tool};
use serde_json::{json, Value};

pub(crate) const TOOLS: &[Tool] = &[
    Tool {
        name: "zazzle_overview",
        description: "Compact map of the git work tree from zazzle-csharp-indexer's index: file and line counts by language, top-level directories, entry points and project files, largest files. Start here on an unfamiliar repository.",
        schema: overview_schema,
        map: overview_args,
    },
    Tool {
        name: "zazzle_find_symbol",
        description: "Find declarations by name from zazzle-csharp-indexer's index (case-insensitive; substring unless exact=true): `path:line kind container.name | signature`, exact matches first. Use it to find where a type or member is declared; zazzle_refs for where it is used.",
        schema: find_symbol_schema,
        map: find_symbol_args,
    },
    Tool {
        name: "zazzle_changed_since",
        description: "Files added (A), modified (M) or deleted (D) in the git work tree since a cursor from an earlier call; call without a cursor to get one. Use it to refresh your picture after edits or a build instead of listing again. Cursors expire when the indexer's daemon restarts.",
        schema: changed_since_schema,
        map: changed_since_args,
    },
    Tool {
        name: "zazzle_git_log",
        description: "Recent commits as `hash date author subject`, optionally for one path, with an optional message filter. Use it instead of git log.",
        schema: git_log_schema,
        map: git_log_args,
    },
    Tool {
        name: "zazzle_git_blame",
        description: "Who last changed each line of a range of a file: `hash (author date line) text`. Use it instead of git blame.",
        schema: git_blame_schema,
        map: git_blame_args,
    },
    Tool {
        name: "zazzle_report_gap",
        description: "Tell the zazzle maintainers what you wanted to know about the code but could not get from these tools, or what was slow, wrong or too verbose. One or two sentences; no code or secrets.",
        schema: report_gap_schema,
        map: report_gap_args,
    },
];

// Schema for zazzle_overview.
fn overview_schema() -> Value {
    object(json!({}), &[])
}

// Schema for zazzle_find_symbol.
fn find_symbol_schema() -> Value {
    object(
        json!({
            "name": {"type": "string", "description": "Symbol name or part of it"},
            "exact": {"type": "boolean", "description": "Whole-name match only"},
            "kind": {"type": "string", "description": "Filter by kind: class, method, property, field, fn, def ..."},
            "limit": {"type": "integer", "description": "Max results (default 30)"},
            "offset": {"type": "integer", "description": "Skip this many results (default 0)"}
        }),
        &["name"],
    )
}

// Schema for zazzle_changed_since.
fn changed_since_schema() -> Value {
    object(json!({"cursor": {"type": "string", "description": "Cursor from a previous call"}}), &[])
}

// Schema for zazzle_git_log.
fn git_log_schema() -> Value {
    object(
        json!({
            "path": {"type": "string", "description": "File or directory to limit history to"},
            "n": {"type": "integer", "description": "Number of commits (default 20)"},
            "grep": {"type": "string", "description": "Only commits whose message matches"}
        }),
        &[],
    )
}

// Schema for zazzle_git_blame.
fn git_blame_schema() -> Value {
    object(
        json!({
            "path": {"type": "string"},
            "start_line": {"type": "integer", "description": "First line (default 1)"},
            "end_line": {"type": "integer", "description": "Last line (default start+39)"}
        }),
        &["path"],
    )
}

// Schema for zazzle_report_gap.
fn report_gap_schema() -> Value {
    object(json!({"what": {"type": "string", "description": "What was missing or wrong"}}), &["what"])
}

// Maps zazzle_overview arguments to a command line.
fn overview_args(_: &Value) -> Result<Vec<String>, String> {
    Ok(vec!["overview".to_string()])
}

// Maps zazzle_find_symbol arguments to a command line.
fn find_symbol_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["find-symbol".to_string()];
    push_flag(&mut a, v, "exact", "--exact")?;
    if let Some(kind) = get_str(v, "kind")? {
        a.extend(["--kind".to_string(), kind]);
    }
    push_num(&mut a, v, "limit", "--limit")?;
    push_num(&mut a, v, "offset", "--offset")?;
    a.extend(["--".to_string(), need_str(v, "name")?]);
    Ok(a)
}

// Maps zazzle_changed_since arguments to a command line.
fn changed_since_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["changed-since".to_string()];
    if let Some(cursor) = get_str(v, "cursor")? {
        a.extend(["--".to_string(), cursor]);
    }
    Ok(a)
}

// Maps zazzle_git_log arguments to a command line.
fn git_log_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["git-log".to_string()];
    push_num(&mut a, v, "n", "-n")?;
    if let Some(g) = get_str(v, "grep")? {
        a.extend(["--grep".to_string(), g]);
    }
    if let Some(p) = get_str(v, "path")? {
        a.extend(["--".to_string(), p]);
    }
    Ok(a)
}

// Maps zazzle_git_blame arguments to a command line.
fn git_blame_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["git-blame".to_string()];
    push_num(&mut a, v, "start_line", "--start")?;
    push_num(&mut a, v, "end_line", "--end")?;
    a.extend(["--".to_string(), need_str(v, "path")?]);
    Ok(a)
}

// Maps zazzle_report_gap arguments to a command line.
fn report_gap_args(v: &Value) -> Result<Vec<String>, String> {
    Ok(vec!["report-gap".to_string(), "--".to_string(), need_str(v, "what")?])
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Maps one tool's arguments to its command line, or the argument error.
    fn map(name: &str, args: Value) -> Result<Vec<String>, String> {
        let tool = TOOLS.iter().find(|t| t.name == name).expect("tool");
        (tool.map)(&args)
    }

    // Checks the structure tools map their arguments to command lines.
    #[test]
    fn test_structure_tools_map_arguments() {
        assert_eq!(map("zazzle_overview", json!({})), Ok(vec!["overview".to_string()]));
        assert_eq!(
            map(
                "zazzle_find_symbol",
                json!({"name":"-Spin","exact":true,"kind":"method","limit":5,"offset":2})
            )
            .expect("args"),
            ["find-symbol", "--exact", "--kind", "method", "--limit", "5", "--offset", "2", "--", "-Spin"]
        );
        assert_eq!(
            map("zazzle_find_symbol", json!({"name":"Spin"})).expect("args"),
            ["find-symbol", "--", "Spin"]
        );
        assert_eq!(map("zazzle_changed_since", json!({})).expect("args"), ["changed-since"]);
        assert_eq!(
            map("zazzle_changed_since", json!({"cursor":"-e;r=1"})).expect("args"),
            ["changed-since", "--", "-e;r=1"]
        );
        assert_eq!(
            map("zazzle_git_log", json!({"path":"src","n":3,"grep":"fix"})).expect("args"),
            ["git-log", "-n", "3", "--grep", "fix", "--", "src"]
        );
        assert_eq!(map("zazzle_git_log", json!({})).expect("args"), ["git-log"]);
        assert_eq!(
            map("zazzle_git_blame", json!({"path":"a.cs","start_line":2,"end_line":4})).expect("args"),
            ["git-blame", "--start", "2", "--end", "4", "--", "a.cs"]
        );
        assert_eq!(
            map("zazzle_report_gap", json!({"what":"-no callers"})).expect("args"),
            ["report-gap", "--", "-no callers"]
        );
    }

    // Checks argument validation and that every schema is an object with a required list.
    #[test]
    fn test_structure_tool_errors_and_schemas() {
        let text = |name: &str, args: Value| format!("error: {}", map(name, args).unwrap_err());
        assert_eq!(text("zazzle_find_symbol", json!({})), "error: 'name' is required");
        assert_eq!(
            text("zazzle_find_symbol", json!({"name": "a", "kind": 1})),
            "error: 'kind' must be a string"
        );
        assert_eq!(text("zazzle_changed_since", json!({"cursor": 1})), "error: 'cursor' must be a string");
        assert_eq!(text("zazzle_git_log", json!({"grep": 1})), "error: 'grep' must be a string");
        assert_eq!(text("zazzle_git_log", json!({"path": 1})), "error: 'path' must be a string");
        assert_eq!(text("zazzle_git_log", json!({"n": -1})), "error: 'n' must be a non-negative integer");
        assert_eq!(text("zazzle_find_symbol", json!({"exact": 1})), "error: 'exact' must be true or false");
        assert_eq!(
            text("zazzle_find_symbol", json!({"limit": "x"})),
            "error: 'limit' must be a non-negative integer"
        );
        assert_eq!(
            text("zazzle_find_symbol", json!({"offset": -1})),
            "error: 'offset' must be a non-negative integer"
        );
        assert_eq!(text("zazzle_git_blame", json!({})), "error: 'path' is required");
        assert_eq!(
            text("zazzle_git_blame", json!({"start_line": -1})),
            "error: 'start_line' must be a non-negative integer"
        );
        assert_eq!(
            text("zazzle_git_blame", json!({"end_line": "x"})),
            "error: 'end_line' must be a non-negative integer"
        );
        assert_eq!(text("zazzle_report_gap", json!({})), "error: 'what' is required");
        for t in TOOLS {
            assert_eq!((t.schema)()["type"], "object", "{}", t.name);
            assert!((t.schema)()["required"].is_array());
        }
    }
}
