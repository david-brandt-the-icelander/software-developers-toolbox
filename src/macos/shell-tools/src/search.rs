// The search operation: a regex search through the daemon's walk and scan, rendered as grouped, capped JSON.
use crate::engine::Ctx;
use crate::fsio;
use crate::host::{display, hidden, resolve, Matches};
use crate::req::SearchArgs;
use crate::state::{Res, State};
use memchr::memchr_iter;
use regex::bytes::{Regex, RegexBuilder};
use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_MATCHES: usize = 100;
const MAX_TEXT: usize = 300;

// One matching line.
struct Hit {
    line: usize,
    col: usize,
    text: String,
}

// A run of consecutive lines around one or more hits.
struct Block {
    start: usize,
    lines: Vec<String>,
    hits: Vec<usize>,
}

// Everything found in one file.
struct FileHits {
    path: String,
    count: usize,
    hits: Vec<Hit>,
    blocks: Vec<Block>,
}

// Return a line as text, cutting it at the display limit.
fn clip(line: &[u8]) -> String {
    if line.len() <= MAX_TEXT {
        return String::from_utf8_lossy(line).into_owned();
    }
    format!("{}…", String::from_utf8_lossy(&line[..MAX_TEXT]))
}

// Compile the pattern as a line-anchored byte regex.
fn build_regex(a: &SearchArgs) -> Res<Regex> {
    RegexBuilder::new(&regex_pattern(a))
        .case_insensitive(a.ignore_case)
        .multi_line(true)
        .build()
        .map_err(|e| format!("bad pattern: {e}"))
}

// Validate the requested hit fields; one field yields compact arrays.
fn parse_fields(a: &SearchArgs) -> Res<Vec<String>> {
    let wanted = a.fields.clone().unwrap_or_else(|| vec!["line".into(), "text".into()]);
    let fields: Vec<String> = wanted.into_iter().filter(|f| f != "path").collect();
    match fields.iter().find(|f| !["line", "col", "text"].contains(&f.as_str())) {
        Some(bad) => Err(format!("unknown field {bad}; use path, line, col, text")),
        None if fields.is_empty() => Err("fields must include line, col or text".to_string()),
        None => Ok(fields),
    }
}

// Returns the byte offset where each line starts, and the number of lines (a final newline adds none).
fn line_starts(bytes: &[u8]) -> (Vec<usize>, usize) {
    let mut starts = vec![0usize];
    starts.extend(memchr_iter(b'\n', bytes).map(|i| i + 1));
    let last = if bytes.last() == Some(&b'\n') { starts.len() - 1 } else { starts.len() };
    (starts, last)
}

// Returns 1-based line n as clipped text.
fn line_text(bytes: &[u8], starts: &[usize], n: usize) -> String {
    let s = starts[n - 1];
    let e = if n < starts.len() { starts[n] - 1 } else { bytes.len() };
    clip(&bytes[s..e])
}

// Group hits into merged context windows of the given radius.
fn build_blocks(bytes: &[u8], hits: &[Hit], radius: usize) -> Vec<Block> {
    let (starts, last) = line_starts(bytes);
    let mut windows: Vec<(usize, usize, Vec<usize>)> = vec![];
    for h in hits {
        let (lo, hi) = (h.line.saturating_sub(radius).max(1), (h.line + radius).min(last));
        match windows.last_mut() {
            Some(w) if lo <= w.1 + 1 => {
                w.1 = w.1.max(hi);
                w.2.push(h.line);
            }
            _ => windows.push((lo, hi, vec![h.line])),
        }
    }
    windows
        .into_iter()
        .map(|(lo, hi, hit_lines)| Block {
            start: lo,
            lines: (lo..=hi).map(|n| line_text(bytes, &starts, n)).collect(),
            hits: hit_lines,
        })
        .collect()
}

// Reads a matched file again to give its hits their text and context; None when it can no longer be read.
fn detail(m: &Matches, shown: String, radius: usize) -> Option<FileHits> {
    let (_, bytes) = fsio::load(&m.path).ok()?;
    let (starts, last) = line_starts(&bytes);
    let hits: Vec<Hit> = m
        .hits
        .iter()
        .filter(|(line, _)| *line <= last)
        .map(|&(line, col)| Hit { line, col, text: line_text(&bytes, &starts, line) })
        .collect();
    let blocks = if radius > 0 { build_blocks(&bytes, &hits, radius) } else { vec![] };
    Some(FileHits { path: shown, count: m.count, hits, blocks })
}

// Render one hit with the chosen fields.
fn render_hit(h: &Hit, fields: &[String]) -> Value {
    let get = |f: &str| match f {
        "line" => json!(h.line),
        "col" => json!(h.col),
        _ => json!(h.text),
    };
    if fields.len() == 1 {
        return get(&fields[0]);
    }
    Value::Object(fields.iter().map(|f| (f.clone(), get(f))).collect())
}

// Render files into result items, spending the remaining match budget.
fn render(files: Vec<FileHits>, fields: &[String], budget: usize) -> (Vec<Value>, bool) {
    let (mut left, mut truncated, mut items) = (budget, false, vec![]);
    for f in files {
        if left == 0 {
            return (items, true);
        }
        truncated |= f.count > f.hits.len();
        if f.blocks.is_empty() {
            let take = f.hits.len().min(left);
            truncated |= take < f.hits.len();
            left -= take;
            let hits: Vec<Value> = f.hits[..take].iter().map(|h| render_hit(h, fields)).collect();
            items.push(json!({"path": f.path, "hits": hits}));
            continue;
        }
        let mut blocks = vec![];
        for b in f.blocks {
            if left == 0 {
                truncated = true;
                break;
            }
            left -= b.hits.len().min(left);
            blocks.push(json!({"start": b.start, "lines": b.lines, "hits": b.hits}));
        }
        items.push(json!({"path": f.path, "blocks": blocks}));
    }
    (items, truncated)
}

// Return the pattern text as a regex, escaping it when literal.
fn regex_pattern(a: &SearchArgs) -> String {
    if a.literal {
        regex::escape(&a.pattern)
    } else {
        a.pattern.clone()
    }
}

// Walk, scan and build the reply.
fn compute(ctx: &Ctx, a: &SearchArgs) -> Res<Value> {
    let re = build_regex(a)?;
    let fields = parse_fields(a)?;
    let cap = a.max_matches.unwrap_or(MAX_MATCHES);
    let roots: Vec<PathBuf> = match a.paths.is_empty() {
        true => vec![ctx.cwd.to_path_buf()],
        false => a.paths.iter().map(|p| resolve(ctx.cwd, p)).collect(),
    };
    let mut files = ctx.host.files(&roots, &a.glob, a.no_ignore)?;
    files.retain(|f| a.hidden || !hidden(f, &roots));
    let found = ctx.host.search(&files, &re, cap);
    let mut files: Vec<FileHits> =
        found.iter().filter_map(|m| detail(m, display(&m.path, ctx.cwd), a.context)).collect();
    files.sort_by(|x, y| x.path.cmp(&y.path));
    let total: usize = files.iter().map(|f| f.count).sum();
    let matched = files.len();
    if a.files_only {
        let paths: Vec<&String> = files.iter().map(|f| &f.path).collect();
        return Ok(json!({"files": paths, "files_matched": matched, "total_matches": total}));
    }
    let (results, truncated) = render(files, &fields, cap);
    let mut v = json!({"results": results, "files_matched": matched, "total_matches": total});
    if truncated {
        v["truncated"] = json!(true);
    }
    Ok(v)
}

// Run a search request, answering unchanged when this session already has the same result.
pub fn run(st: &mut State, ctx: &Ctx, a: SearchArgs) -> Res<Value> {
    let value = compute(ctx, &a)?;
    let fresh = a.fresh;
    let keyed = SearchArgs { fresh: false, ..a };
    Ok(st.serve(&format!("search|{}|{keyed:?}", ctx.cwd.display()), fresh, value))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::host::testing::Plain;
    use crate::state::{Sent, Shared};
    use std::path::Path;

    // Overlapping context windows merge into one block.
    #[test]
    fn test_build_blocks_merges() {
        let data = b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let hits = vec![
            Hit { line: 2, col: 1, text: "b".into() },
            Hit { line: 4, col: 1, text: "d".into() },
            Hit { line: 9, col: 1, text: "i".into() },
        ];
        let blocks = build_blocks(data, &hits, 1);
        assert_eq!(blocks.len(), 2);
        assert_eq!((blocks[0].start, blocks[0].lines.len()), (1, 5));
        assert_eq!(blocks[1].hits, vec![9]);
    }

    // Build search arguments from JSON.
    fn args(v: Value) -> SearchArgs {
        serde_json::from_value(v).unwrap()
    }

    // Unknown field names are rejected, path is implied, and an empty list is refused.
    #[test]
    fn test_parse_fields_validation() {
        let mut a = args(json!({"pattern": "x"}));
        assert_eq!(parse_fields(&a).unwrap(), vec!["line", "text"]);
        a.fields = Some(vec!["path".into(), "line".into()]);
        assert_eq!(parse_fields(&a).unwrap(), vec!["line"]);
        a.fields = Some(vec!["bogus".into()]);
        assert!(parse_fields(&a).is_err());
        a.fields = Some(vec![]);
        assert!(parse_fields(&a).is_err());
        a.fields = Some(vec!["path".into()]);
        assert!(parse_fields(&a).is_err());
    }

    // Overlong matching lines are cut for display.
    #[test]
    fn test_clip_long_line() {
        assert_eq!(clip(b"short"), "short");
        let cut = clip(&vec![b'a'; MAX_TEXT + 10]);
        assert!(cut.ends_with('…'));
        assert!(cut.len() < MAX_TEXT + 10);
    }

    // Context blocks work for hits on an unterminated last line.
    #[test]
    fn test_build_blocks_without_trailing_newline() {
        let hits = vec![Hit { line: 3, col: 1, text: "c".into() }];
        let blocks = build_blocks(b"a\nb\nc", &hits, 5);
        assert_eq!((blocks[0].start, blocks[0].lines.join("")), (1, "abc".to_string()));
    }

    // A file that vanished after the scan is dropped, and hits past its end are ignored.
    #[test]
    fn test_detail_missing_and_shrunk() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, "one\n").unwrap();
        let m = Matches { path: f.clone(), count: 2, hits: vec![(1, 1), (5, 1)] };
        assert_eq!(detail(&m, "f".into(), 0).unwrap().hits.len(), 1);
        std::fs::remove_file(&f).unwrap();
        assert!(detail(&m, "f".into(), 0).is_none());
    }

    // Build a FileHits with the given number of plain hits.
    fn file_hits(path: &str, n: usize) -> FileHits {
        let hits = (0..n).map(|i| Hit { line: i + 1, col: 1, text: "t".into() }).collect();
        FileHits { path: path.into(), count: n, hits, blocks: vec![] }
    }

    // Rendering stops at the budget and says it truncated, across files and across blocks.
    #[test]
    fn test_render_budget() {
        let fields = vec!["line".to_string()];
        let (items, truncated) = render(vec![file_hits("a", 1), file_hits("b", 1)], &fields, 1);
        assert_eq!((items.len(), truncated), (1, true));
        let (items, truncated) = render(vec![file_hits("a", 3)], &fields, 2);
        assert_eq!((items[0]["hits"].as_array().unwrap().len(), truncated), (2, true));
        let block = |start: usize| Block { start, lines: vec!["l".into()], hits: vec![start] };
        let two_blocks =
            FileHits { path: "a".into(), count: 2, hits: vec![], blocks: vec![block(1), block(5)] };
        let (items, truncated) = render(vec![two_blocks], &fields, 1);
        assert_eq!((items[0]["blocks"].as_array().unwrap().len(), truncated), (1, true));
        let (_, fine) = render(vec![file_hits("a", 2)], &fields, 5);
        assert!(!fine);
        let mut capped = file_hits("a", 1);
        capped.count = 4;
        assert!(render(vec![capped], &fields, 5).1);
    }

    // Hits render as objects when several fields are asked for.
    #[test]
    fn test_render_hit_objects() {
        let h = Hit { line: 4, col: 2, text: "t".into() };
        let all = ["line", "col", "text"].map(String::from);
        assert_eq!(render_hit(&h, &all), json!({"line": 4, "col": 2, "text": "t"}));
    }

    // Runs a search request in a directory with a fresh session.
    fn search(cwd: &Path, v: Value) -> Res<Value> {
        let (mut sh, mut sent) = (Shared::default(), Sent::default());
        run(&mut State { shared: &mut sh, sent: &mut sent }, &Ctx { cwd, host: &Plain }, args(v))
    }

    // Create a small project with a hidden file.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\nfn beta() {}\n// TODO one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing\nTODO two\n").unwrap();
        std::fs::write(dir.path().join(".h"), "TODO hidden\n").unwrap();
        dir
    }

    // Search supports context blocks, files_only, compact fields, caps, hidden files and paths.
    #[test]
    fn test_search_shapes() {
        let dir = project();
        let cwd = dir.path();
        let ctx = search(cwd, json!({"pattern": "alpha", "context": 1})).unwrap();
        assert_eq!(ctx["results"][0]["blocks"][0]["lines"].as_array().unwrap().len(), 2);
        let files = search(cwd, json!({"pattern": "TODO", "files_only": true})).unwrap();
        assert_eq!(files["files"], json!(["a.rs", "b.txt"]));
        let lines = search(cwd, json!({"pattern": "todo", "ignore_case": true, "fields": ["line"]})).unwrap();
        assert_eq!(lines["results"][0]["hits"], json!([3]));
        let cap = search(cwd, json!({"pattern": "TODO", "max_matches": 1})).unwrap();
        assert_eq!(cap["truncated"], true);
        assert_eq!(search(cwd, json!({"pattern": "TODO", "hidden": true})).unwrap()["files_matched"], 3);
        let glob = search(cwd, json!({"pattern": "TODO", "paths": ["."], "glob": ["*.txt"]})).unwrap();
        assert_eq!(glob["results"][0]["path"], "b.txt");
        assert_eq!(search(cwd, json!({"pattern": "a.b", "literal": true})).unwrap()["total_matches"], 0);
    }

    // Bad patterns, bad fields and missing paths are errors.
    #[test]
    fn test_search_errors() {
        let dir = project();
        assert!(search(dir.path(), json!({"pattern": "("})).unwrap_err().contains("bad pattern"));
        assert!(search(dir.path(), json!({"pattern": "x", "fields": ["no"]})).is_err());
        assert!(search(dir.path(), json!({"pattern": "x", "paths": ["missing"]})).is_err());
    }
}
