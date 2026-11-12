// The search operation: parallel, ignore-aware regex search with grouped, capped, cached results.
use crate::fsio::{self, Ident, Loaded};
use crate::index::{self, TriIndex, Update};
use crate::par;
use crate::req::SearchArgs;
use crate::state::{Dep, Res, State};
use crate::walk;
use ignore::WalkState;
use memchr::{memchr, memchr_iter, memrchr};
use regex::bytes::{Regex, RegexBuilder};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_MATCHES: usize = 100;
const MAX_TEXT: usize = 300;
const CHUNK: usize = 32;

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

// Group hits into merged context windows of the given radius.
fn build_blocks(bytes: &[u8], hits: &[Hit], radius: usize) -> Vec<Block> {
    let mut starts = vec![0usize];
    starts.extend(memchr_iter(b'\n', bytes).map(|i| i + 1));
    let last = if bytes.last() == Some(&b'\n') { starts.len() - 1 } else { starts.len() };
    let text = |n: usize| {
        let s = starts[n - 1];
        let e = if n < starts.len() { starts[n] - 1 } else { bytes.len() };
        clip(&bytes[s..e])
    };
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
        .map(|(lo, hi, hit_lines)| Block { start: lo, lines: (lo..=hi).map(text).collect(), hits: hit_lines })
        .collect()
}

// Scan file bytes and return the hits, or None when there are none.
fn scan_bytes(path: &Path, bytes: &[u8], re: &Regex, radius: usize, cap: usize) -> Option<FileHits> {
    let (mut count, mut hits, mut line, mut counted_to, mut at) = (0, vec![], 1usize, 0usize, 0usize);
    while at < bytes.len() {
        let Some(m) = re.find_at(bytes, at) else { break };
        if m.start() >= bytes.len() {
            break;
        }
        let ls = memrchr(b'\n', &bytes[..m.start()]).map_or(0, |i| i + 1);
        let le = memchr(b'\n', &bytes[m.start()..]).map_or(bytes.len(), |i| m.start() + i);
        line += memchr_iter(b'\n', &bytes[counted_to..ls]).count();
        counted_to = ls;
        count += 1;
        if hits.len() < cap {
            hits.push(Hit { line, col: m.start() - ls + 1, text: clip(&bytes[ls..le]) });
        }
        at = le + 1;
    }
    if count == 0 {
        return None;
    }
    let blocks = if radius > 0 { build_blocks(bytes, &hits, radius) } else { vec![] };
    Some(FileHits { path: walk::display(path), count, hits, blocks })
}

// Scan a loaded file, skipping binary and unreadable files.
fn scan(path: &Path, loaded: Loaded, re: &Regex, radius: usize, cap: usize) -> Option<FileHits> {
    let (_, bytes) = loaded.ok()?;
    if fsio::is_binary(&bytes) {
        return None;
    }
    scan_bytes(path, &bytes, re, radius, cap)
}

// What reading a new or changed file produced: its index update and any hits.
struct Seen {
    ident: Ident,
    at: i128,
    update: Update,
    hits: Option<FileHits>,
}

// Classify a loaded file for the index and scan it.
fn read_and_scan(path: &Path, walked: Ident, loaded: Loaded, re: &Regex, radius: usize, cap: usize) -> Seen {
    let Ok((ident, bytes)) = loaded else {
        return Seen { ident: walked, at: fsio::now_ns(), update: Update::Always, hits: None };
    };
    let at = fsio::now_ns();
    let binary = fsio::is_binary(&bytes);
    let update = Update::classify(&bytes, &ident, at, binary);
    let hits = if binary { None } else { scan_bytes(path, &bytes, re, radius, cap) };
    Seen { ident, at, update, hits }
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

// A walked file path with its stamp.
type Walked = (PathBuf, Ident);

// Walk the roots in parallel and return the files with stamps, plus every stamp the result depends on.
fn walk_files(a: &SearchArgs) -> Res<(Vec<Walked>, Vec<Dep>)> {
    let walker = walk::builder(&a.paths, &a.glob, a.hidden, a.no_ignore, None)?;
    let files: Mutex<Vec<Walked>> = Mutex::new(vec![]);
    let deps: Mutex<Vec<Dep>> = Mutex::new(vec![]);
    walker.build_parallel().run(|| {
        Box::new(|entry| {
            let Some((entry, md)) = entry.ok().and_then(|e| e.metadata().ok().map(|m| (e, m))) else {
                return WalkState::Continue;
            };
            let ident = Ident::of(&md);
            let mut mine: Vec<Dep> = vec![(entry.path().to_path_buf(), Some(ident))];
            if md.is_dir() {
                mine.extend(walk::ignore_file_deps(entry.path()));
            }
            deps.lock().expect("deps lock").extend(mine);
            if md.is_file() {
                files.lock().expect("files lock").push((entry.path().to_path_buf(), ident));
            }
            WalkState::Continue
        })
    });
    let mut files = files.into_inner().expect("files lock");
    files.sort_by(|x, y| x.0.cmp(&y.0));
    Ok((files, deps.into_inner().expect("deps lock")))
}

// Load and scan files on all cores, batching the reads, and return the files with hits.
fn scan_all(paths: &[&PathBuf], re: &Regex, radius: usize, cap: usize) -> Vec<FileHits> {
    let scanned = par::map_chunks(paths, CHUNK, |chunk| {
        let refs: Vec<&Path> = chunk.iter().map(|p| p.as_path()).collect();
        let loaded = fsio::load_many(&refs);
        chunk.iter().zip(loaded).map(|(p, l)| scan(p, l, re, radius, cap)).collect()
    });
    scanned.into_iter().flatten().collect()
}

// Find the hits in all files, reading only index candidates plus files the index has not seen.
fn find_hits(
    a: &SearchArgs,
    re: &Regex,
    files: &[Walked],
    index: &mut TriIndex,
    cap: usize,
) -> Vec<FileHits> {
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(a.ignore_case)
        .multi_line(true)
        .build()
        .parse(&regex_pattern(a));
    let plan = hir.ok().and_then(|h| index::plan(&h));
    let mut found: Vec<FileHits> = vec![];
    if !index.usable() || !index.warm() {
        let paths: Vec<&PathBuf> = files.iter().map(|f| &f.0).collect();
        return scan_all(&paths, re, a.context, cap);
    }
    let stale = index.begin(files);
    let todo: Vec<&Walked> = stale.iter().map(|&i| &files[i]).collect();
    let seen = par::map_chunks(&todo, CHUNK, |c| {
        let paths: Vec<&Path> = c.iter().map(|f| f.0.as_path()).collect();
        let loaded = fsio::load_many(&paths);
        c.iter().zip(loaded).map(|((p, id), l)| read_and_scan(p, *id, l, re, a.context, cap)).collect()
    });
    let processed: HashSet<&PathBuf> = todo.iter().map(|f| &f.0).collect();
    for ((path, _), s) in todo.iter().zip(seen) {
        found.extend(s.hits);
        index.apply(path, s.ident, s.at, s.update);
    }
    index.finish();
    if !index.usable() {
        return find_hits(a, re, files, index, cap);
    }
    let candidates: Vec<PathBuf> =
        index.candidates(plan.as_deref()).into_iter().filter(|p| !processed.contains(p)).collect();
    let candidates: Vec<&PathBuf> = candidates.iter().collect();
    found.extend(scan_all(&candidates, re, a.context, cap));
    found
}

// Return the pattern text as a regex, escaping it when literal.
fn regex_pattern(a: &SearchArgs) -> String {
    if a.literal {
        regex::escape(&a.pattern)
    } else {
        a.pattern.clone()
    }
}

// Walk, find hits through the index and build the reply with its dependencies.
fn compute(a: &SearchArgs, index: &mut TriIndex) -> Res<(Value, Vec<Dep>)> {
    let re = build_regex(a)?;
    let fields = parse_fields(a)?;
    let cap = a.max_matches.unwrap_or(MAX_MATCHES);
    let (walked, deps) = walk_files(a)?;
    let mut files = find_hits(a, &re, &walked, index, cap);
    files.sort_by(|x, y| x.path.cmp(&y.path));
    let total: usize = files.iter().map(|f| f.count).sum();
    let matched = files.len();
    let value = if a.files_only {
        let paths: Vec<&String> = files.iter().map(|f| &f.path).collect();
        json!({"files": paths, "files_matched": matched, "total_matches": total})
    } else {
        let (results, truncated) = render(files, &fields, cap);
        let mut v = json!({"results": results, "files_matched": matched, "total_matches": total});
        if truncated {
            v["truncated"] = json!(true);
        }
        v
    };
    Ok((value, deps))
}

// Run a search request through the session cache.
pub fn run(st: &mut State, a: SearchArgs) -> Res<Value> {
    let mut keyed = a.clone();
    keyed.fresh = false;
    let config = format!("{:?}|{:?}|{}|{}", a.paths, a.glob, a.hidden, a.no_ignore);
    let mut index = st.take_index(&config);
    let reply = st.serve_cached(&format!("search|{keyed:?}"), a.fresh, || compute(&a, &mut index));
    st.put_index(config, index);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // Unknown field names are rejected and path is implied.
    #[test]
    fn test_parse_fields_validation() {
        let mut a = SearchArgs {
            pattern: "x".into(),
            literal: false,
            ignore_case: false,
            paths: vec![],
            glob: vec![],
            context: 0,
            max_matches: None,
            files_only: false,
            fields: None,
            hidden: false,
            no_ignore: false,
            fresh: false,
        };
        assert_eq!(parse_fields(&a).unwrap(), vec!["line", "text"]);
        a.fields = Some(vec!["path".into(), "line".into()]);
        assert_eq!(parse_fields(&a).unwrap(), vec!["line"]);
        a.fields = Some(vec!["bogus".into()]);
        assert!(parse_fields(&a).is_err());
    }

    // Scanning reports line, column, ignores binary files and counts every match.
    #[test]
    fn test_scan_lines_and_binary() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "one\ntwo foo\nthree\nfoo foo\n").unwrap();
        let re = RegexBuilder::new("foo").multi_line(true).build().unwrap();
        let r = scan(&f, fsio::load(&f), &re, 0, 10).unwrap();
        assert_eq!((r.count, r.hits[0].line, r.hits[0].col, r.hits[1].line), (2, 2, 5, 4));
        let b = dir.path().join("b.bin");
        std::fs::write(&b, b"foo\0bar").unwrap();
        assert!(scan(&b, fsio::load(&b), &re, 0, 10).is_none());
    }

    // Build search arguments from JSON.
    fn args(v: serde_json::Value) -> SearchArgs {
        serde_json::from_value(v).unwrap()
    }

    // Compile a multi-line byte regex.
    fn re(p: &str) -> Regex {
        RegexBuilder::new(p).multi_line(true).build().unwrap()
    }

    // Overlong matching lines are cut for display.
    #[test]
    fn test_clip_long_line() {
        assert_eq!(clip(b"short"), "short");
        let cut = clip(&vec![b'a'; MAX_TEXT + 10]);
        assert!(cut.ends_with('…'));
        assert!(cut.len() < MAX_TEXT + 10);
    }

    // An empty field list is rejected whether given directly or as only path.
    #[test]
    fn test_parse_fields_empty() {
        let mut a = args(json!({"pattern": "x", "fields": []}));
        assert!(parse_fields(&a).is_err());
        a.fields = Some(vec!["path".into()]);
        assert!(parse_fields(&a).is_err());
    }

    // Context blocks work for hits on an unterminated last line.
    #[test]
    fn test_build_blocks_without_trailing_newline() {
        let hits = vec![Hit { line: 3, col: 1, text: "c".into() }];
        let blocks = build_blocks(b"a\nb\nc", &hits, 5);
        assert_eq!((blocks[0].start, blocks[0].lines.join("")), (1, "abc".to_string()));
    }

    // Scanning handles an unterminated last line, EOF-anchored matches, the hit cap and empty input.
    #[test]
    fn test_scan_bytes_edges() {
        let p = Path::new("f");
        let last = scan_bytes(p, b"x\nfoo", &re("foo"), 0, 10).unwrap();
        assert_eq!((last.count, last.hits[0].line), (1, 2));
        let eof = scan_bytes(p, b"a\n", &re("$"), 0, 10).unwrap();
        assert_eq!(eof.count, 1);
        let capped = scan_bytes(p, b"foo\nfoo\nfoo\n", &re("foo"), 0, 1).unwrap();
        assert_eq!((capped.count, capped.hits.len()), (3, 1));
        assert!(scan_bytes(p, b"", &re("x*"), 0, 10).is_none());
        assert!(scan_bytes(p, b"ab", &re("$"), 0, 10).is_none());
        assert!(scan_bytes(p, b"abc", &re("zzz"), 0, 10).is_none());
    }

    // A failed load still tells the index to always scan the file.
    #[test]
    fn test_read_and_scan_failed_load() {
        let dir = tempfile::tempdir().unwrap();
        let id = fsio::ident_of(dir.path()).unwrap();
        let seen = read_and_scan(Path::new("x"), id, Err(std::io::Error::other("boom")), &re("x"), 0, 5);
        assert!(seen.hits.is_none());
        assert!(matches!(seen.update, Update::Always));
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
    }

    // Hits render as objects when several fields are asked for.
    #[test]
    fn test_render_hit_objects() {
        let h = Hit { line: 4, col: 2, text: "t".into() };
        let all = ["line", "col", "text"].map(String::from);
        assert_eq!(render_hit(&h, &all), json!({"line": 4, "col": 2, "text": "t"}));
    }

    // Create a small project with a few source files.
    fn project() -> (tempfile::TempDir, Vec<Walked>) {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..6 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), format!("alpha {i}\nneedle here\n"))
                .unwrap();
        }
        let a = args(json!({"pattern": "x", "paths": [dir.path()]}));
        let (files, _) = walk_files(&a).unwrap();
        (dir, files)
    }

    // A disabled index and an index that overflows mid-search both fall back to a full scan.
    #[test]
    fn test_find_hits_with_unusable_index() {
        let (dir, files) = project();
        std::thread::sleep(std::time::Duration::from_millis(2200));
        let a = args(json!({"pattern": "needle", "paths": [dir.path()]}));
        let regex = build_regex(&a).unwrap();
        let mut tiny = TriIndex::with_limit(1);
        tiny.warm();
        assert_eq!(find_hits(&a, &regex, &files, &mut tiny, 10).len(), 6);
        assert!(!tiny.usable());
        assert_eq!(find_hits(&a, &regex, &files, &mut tiny, 10).len(), 6);
    }

    // The first search skips the index and later ones use it with equal results.
    #[test]
    fn test_find_hits_cold_then_warm() {
        let (dir, files) = project();
        let a = args(json!({"pattern": "needle", "paths": [dir.path()]}));
        let regex = build_regex(&a).unwrap();
        let mut idx = TriIndex::new();
        let cold = find_hits(&a, &regex, &files, &mut idx, 10);
        let warm = find_hits(&a, &regex, &files, &mut idx, 10);
        assert_eq!((cold.len(), warm.len()), (6, 6));
    }

    // Literal patterns are escaped and bad regexes are reported.
    #[test]
    fn test_regex_pattern_and_errors() {
        assert_eq!(regex_pattern(&args(json!({"pattern": "a.b", "literal": true}))), "a\\.b");
        let bad = run(&mut State::new(), args(json!({"pattern": "(", "paths": ["."]})));
        assert!(bad.unwrap_err().contains("bad pattern"));
        let missing = run(&mut State::new(), args(json!({"pattern": "x", "paths": ["/no/such"]})));
        assert!(missing.is_err());
    }

    // Searching with no paths uses the current directory and a glob narrows the files.
    #[test]
    fn test_run_default_paths_and_glob() {
        let a = args(json!({"pattern": "fn main", "glob": ["main.rs"], "files_only": true}));
        let out = run(&mut State::new(), a).unwrap();
        assert_eq!(out["files"], json!(["src/main.rs"]));
    }
}
