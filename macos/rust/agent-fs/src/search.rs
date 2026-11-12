// The search operation: parallel, ignore-aware regex search with grouped, capped, cached results.
use crate::fsio::{self, Ident};
use crate::req::SearchArgs;
use crate::state::{Dep, Res, State};
use crate::walk;
use ignore::WalkState;
use memchr::{memchr, memchr_iter, memrchr};
use regex::bytes::{Regex, RegexBuilder};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Mutex;

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
    let pattern = if a.literal { regex::escape(&a.pattern) } else { a.pattern.clone() };
    RegexBuilder::new(&pattern)
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

// Scan one file and return its hits, or None when it is binary, unreadable or has none.
fn scan(path: &Path, re: &Regex, radius: usize, cap: usize) -> Option<FileHits> {
    let (_, bytes) = fsio::load(path).ok()?;
    if fsio::is_binary(&bytes) {
        return None;
    }
    let (mut count, mut hits, mut line, mut counted_to, mut at) = (0, vec![], 1usize, 0usize, 0usize);
    while at <= bytes.len() {
        let Some(m) = re.find_at(&bytes, at) else { break };
        if m.start() >= bytes.len() && !bytes.is_empty() {
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
    let blocks = if radius > 0 { build_blocks(&bytes, &hits, radius) } else { vec![] };
    Some(FileHits { path: walk::display(path), count, hits, blocks })
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

// Walk the roots, scan every file in parallel and build the reply with its dependencies.
fn compute(a: &SearchArgs) -> Res<(Value, Vec<Dep>)> {
    let re = build_regex(a)?;
    let fields = parse_fields(a)?;
    let cap = a.max_matches.unwrap_or(MAX_MATCHES);
    let walker = walk::builder(&a.paths, &a.glob, a.hidden, a.no_ignore, None)?;
    let found: Mutex<Vec<FileHits>> = Mutex::new(vec![]);
    let deps: Mutex<Vec<Dep>> = Mutex::new(vec![]);
    walker.build_parallel().run(|| {
        Box::new(|entry| {
            let Ok(entry) = entry else { return WalkState::Continue };
            let Ok(md) = entry.metadata() else { return WalkState::Continue };
            let mut mine: Vec<Dep> = vec![(entry.path().to_path_buf(), Some(Ident::of(&md)))];
            if md.is_dir() {
                mine.extend(walk::ignore_file_deps(entry.path()));
            }
            deps.lock().expect("deps lock").extend(mine);
            if md.is_file() {
                if let Some(f) = scan(entry.path(), &re, a.context, cap) {
                    found.lock().expect("found lock").push(f);
                }
            }
            WalkState::Continue
        })
    });
    let mut files = found.into_inner().expect("found lock");
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
    Ok((value, deps.into_inner().expect("deps lock")))
}

// Run a search request through the session cache.
pub fn run(st: &mut State, a: SearchArgs) -> Res<Value> {
    let mut keyed = a.clone();
    keyed.fresh = false;
    st.serve_cached(&format!("search|{keyed:?}"), a.fresh, || compute(&a))
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
        let r = scan(&f, &re, 0, 10).unwrap();
        assert_eq!((r.count, r.hits[0].line, r.hits[0].col, r.hits[1].line), (2, 2, 5, 4));
        let b = dir.path().join("b.bin");
        std::fs::write(&b, b"foo\0bar").unwrap();
        assert!(scan(&b, &re, 0, 10).is_none());
    }
}
