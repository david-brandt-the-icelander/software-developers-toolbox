// MCP tools for the structure commands answered by zazzle-csharp-indexer: descriptions, schemas and argument mapping.
use crate::mcp::{get_str, need_str, object, push_flag, push_num, Tool};
use serde_json::{json, Value};

pub(crate) const TOOLS: &[Tool] = &[
    Tool {
        name: "zazzle_overview",
        description: "Map of the repository from the C# indexer: languages, top directories, entry and project files, largest files. Start here; in a folder of repositories it lists them.",
        schema: overview_schema,
        map: overview_args,
    },
    Tool {
        name: "zazzle_find_symbol",
        description: "Where a type or member is declared, from the C# indexer (substring unless exact): path:line kind name | signature. zazzle_refs for uses.",
        schema: find_symbol_schema,
        map: find_symbol_args,
    },
    Tool {
        name: "zazzle_changed_since",
        description: "Files added, modified or deleted since a cursor from an earlier call (none: get one); refresh after edits instead of listing again.",
        schema: changed_since_schema,
        map: changed_since_args,
    },
    Tool {
        name: "zazzle_git_log",
        description: "Recent commits (hash date author subject), optionally for a path or a message filter. Use instead of git log.",
        schema: git_log_schema,
        map: git_log_args,
    },
    Tool {
        name: "zazzle_git_blame",
        description: "Who last changed each line of a range. Use instead of git blame.",
        schema: git_blame_schema,
        map: git_blame_args,
    },
    Tool {
        name: "zazzle_report_gap",
        description: "Tell zazzle's maintainers, in a sentence, what you needed and could not get, or what was slow or wrong. No code or secrets.",
        schema: report_gap_schema,
        map: report_gap_args,
    },
];

// Schema for zazzle_overview.
fn overview_schema() -> Value {
    object(json!({"repo": repo_prop()}), &[])
}

// The optional repo argument: which repository, when the working directory holds several.
fn repo_prop() -> Value {
    json!({"type": "string", "description": "Repository, if the folder holds several"})
}

// Schema for zazzle_find_symbol.
fn find_symbol_schema() -> Value {
    object(
        json!({
            "name": {"type": "string", "description": "Name or part; Namespace.Type.Member narrows"},
            "exact": {"type": "boolean", "description": "Whole-name match only"},
            "kind": {"type": "string", "description": "Filter by kind: class, method, property, field, fn, def ..."},
            "limit": {"type": "integer", "description": "Max results (default 30)"},
            "offset": {"type": "integer", "description": "Skip this many results (default 0)"},
            "repo": repo_prop()
        }),
        &["name"],
    )
}

// Schema for zazzle_changed_since.
fn changed_since_schema() -> Value {
    object(
        json!({"cursor": {"type": "string", "description": "Cursor from a previous call"}, "repo": repo_prop()}),
        &[],
    )
}

// Schema for zazzle_git_log.
fn git_log_schema() -> Value {
    object(
        json!({
            "path": {"type": "string", "description": "File or directory to limit history to"},
            "n": {"type": "integer", "description": "Number of commits (default 20)"},
            "grep": {"type": "string", "description": "Only commits whose message matches"},
            "repo": repo_prop()
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
    object(
        json!({"what": {"type": "string", "description": "What was missing or wrong"}, "repo": repo_prop()}),
        &["what"],
    )
}

// Maps zazzle_overview arguments to a command line.
fn overview_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["overview".to_string()];
    push_repo(&mut a, v)?;
    Ok(a)
}

// Adds `--repo DIR` when the repo argument was given.
fn push_repo(a: &mut Vec<String>, v: &Value) -> Result<(), String> {
    if let Some(r) = get_str(v, "repo")? {
        a.extend(["--repo".to_string(), r]);
    }
    Ok(())
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
    push_repo(&mut a, v)?;
    a.extend(["--".to_string(), need_str(v, "name")?]);
    Ok(a)
}

// Maps zazzle_changed_since arguments to a command line.
fn changed_since_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["changed-since".to_string()];
    push_repo(&mut a, v)?;
    if let Some(cursor) = get_str(v, "cursor")? {
        a.extend(["--".to_string(), cursor]);
    }
    Ok(a)
}

// Maps zazzle_git_log arguments to a command line.
fn git_log_args(v: &Value) -> Result<Vec<String>, String> {
    let mut a = vec!["git-log".to_string()];
    push_num(&mut a, v, "n", "-n")?;
    push_repo(&mut a, v)?;
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
    let mut a = vec!["report-gap".to_string()];
    push_repo(&mut a, v)?;
    a.extend(["--".to_string(), need_str(v, "what")?]);
    Ok(a)
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
        let r = json!({"repo": "api"});
        assert_eq!(map("zazzle_overview", r.clone()).expect("args"), ["overview", "--repo", "api"]);
        assert_eq!(
            map("zazzle_find_symbol", json!({"name": "S", "repo": "api"})).expect("args"),
            ["find-symbol", "--repo", "api", "--", "S"]
        );
        assert_eq!(map("zazzle_changed_since", r.clone()).expect("args"), ["changed-since", "--repo", "api"]);
        assert_eq!(map("zazzle_git_log", r).expect("args"), ["git-log", "--repo", "api"]);
        assert_eq!(
            map("zazzle_report_gap", json!({"what": "x", "repo": "api"})).expect("args"),
            ["report-gap", "--repo", "api", "--", "x"]
        );
        assert_eq!(map("zazzle_overview", json!({"repo": 1})).unwrap_err(), "'repo' must be a string");
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
