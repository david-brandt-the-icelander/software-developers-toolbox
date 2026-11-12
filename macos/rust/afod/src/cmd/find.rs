// The find command: regex search over many files with grouped, capped, token-lean output.
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::meta::{line_starts, looks_binary};
use crate::util::{clip, display, resolve, MAX_LINE};
use crate::walk::{self, WalkOpts};
use memchr::{memchr_iter, memrchr};
use regex::bytes::{Regex, RegexBuilder};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const SPEC: &[Flag] = &[
    flag("ignore-case", 'i'),
    flag("fixed", 'F'),
    flag("word", 'w'),
    flag("count", 'c'),
    flag("files", 'l'),
    flag("no-ignore", 'u'),
    opt("context", 'C'),
    opt("glob", 'g'),
    opt("max", 'm'),
];
const USAGE: &str = "usage: find PATTERN [PATH..] [-i -F -w -c -l -u] [-C N] [-g GLOB] [-m N]";
const DEFAULT_MAX: usize = 50;
const LIST_CAP: usize = 200;
const MAX_FILE: u64 = 2 << 20;
const PARALLEL_MIN: usize = 8;

// A matching line: its number and where the match starts within it.
#[derive(Debug)]
struct Hit {
    line: usize,
    col: usize,
}

// Why a file produced no search results.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Skip {
    No,
    Binary,
    Large,
    Unreadable,
}

// The outcome of searching one file.
#[derive(Debug)]
struct FileResult {
    idx: usize,
    total: usize,
    hits: Vec<Hit>,
    skip: Skip,
}

// Parsed search settings.
struct Plan {
    re: Regex,
    max: usize,
    context: usize,
    files_only: bool,
}

// Turns a regex library error into one short line.
fn friendly(e: &regex::Error) -> String {
    let s = e.to_string();
    let msg = s
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("error: "))
        .unwrap_or_else(|| s.lines().next().unwrap_or(""));
    let hint = if msg.contains("look-around") || msg.contains("backreference") {
        "; rewrite without it or use -F"
    } else {
        ""
    };
    format!("invalid regex: {msg}{hint}")
}

// Builds the search regex from the pattern and flags.
fn build_regex(pattern: &str, ignore_case: bool, fixed: bool, word: bool) -> Result<Regex, String> {
    let mut pat = if fixed { regex::escape(pattern) } else { pattern.to_string() };
    if word {
        pat = format!(r"\b(?:{pat})\b");
    }
    RegexBuilder::new(&pat).case_insensitive(ignore_case).multi_line(true).build().map_err(|e| friendly(&e))
}

// Searches one file's bytes and returns the matching lines.
fn search_bytes(data: &[u8], plan: &Plan) -> (usize, Vec<Hit>) {
    let (mut total, mut hits) = (0, Vec::new());
    let (mut line_no, mut cursor, mut last_line) = (1, 0, 0);
    for m in plan.re.find_iter(data) {
        let ls = memrchr(b'\n', &data[..m.start()]).map_or(0, |p| p + 1);
        line_no += memchr_iter(b'\n', &data[cursor..ls]).count();
        cursor = ls;
        if line_no == last_line {
            continue;
        }
        last_line = line_no;
        total += 1;
        if hits.len() < plan.max {
            hits.push(Hit { line: line_no, col: m.start() - ls });
        }
        if plan.files_only {
            break;
        }
    }
    (total, hits)
}

// Reads a file for searching, or says why it is skipped.
fn read_searchable(path: &Path) -> Result<Vec<u8>, Skip> {
    let len = std::fs::metadata(path).map_err(|_| Skip::Unreadable)?.len();
    if len > MAX_FILE {
        return Err(Skip::Large);
    }
    let data = std::fs::read(path).map_err(|_| Skip::Unreadable)?;
    if looks_binary(&data) {
        Err(Skip::Binary)
    } else {
        Ok(data)
    }
}

// Reads and searches one file, classifying files that cannot be searched.
fn search_file(idx: usize, path: &Path, plan: &Plan) -> FileResult {
    let (total, hits, skip) = match read_searchable(path) {
        Ok(data) => {
            let (total, hits) = search_bytes(&data, plan);
            (total, hits, Skip::No)
        }
        Err(skip) => (0, vec![], skip),
    };
    FileResult { idx, total, hits, skip }
}

// Searches all files, in parallel when there are enough, and returns results in file order.
fn search_all(files: &[PathBuf], plan: &Plan, cpus: usize) -> Vec<FileResult> {
    let results = Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    let work = || loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        let Some(path) = files.get(i) else { break };
        let r = search_file(i, path, plan);
        if r.total > 0 || r.skip != Skip::No {
            results.lock().expect("results lock").push(r);
        }
    };
    let workers = cpus.min(files.len());
    if files.len() < PARALLEL_MIN || workers < 2 {
        work();
    } else {
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(work);
            }
        });
    }
    let mut out = results.into_inner().expect("results lock");
    out.sort_by_key(|r| r.idx);
    out
}

// Renders one line as text, clipped around the match column.
fn render_line(data: &[u8], starts: &[u32], n: usize, col: usize) -> String {
    let line = String::from_utf8_lossy(crate::meta::line_bytes(data, starts, n));
    clip(&line, col, MAX_LINE)
}

// Renders one file's hits with merged context windows.
fn render_file(out: &mut String, data: &[u8], hits: &[Hit], context: usize) {
    let starts = line_starts(data);
    let nl = starts.len();
    let mut next_line = 1;
    let mut hit_iter = hits.iter().peekable();
    let mut printed_any = false;
    while let Some(h) = hit_iter.next() {
        let lo = h.line.saturating_sub(context).max(next_line);
        let mut hi = (h.line + context).min(nl);
        let mut group = vec![h];
        while let Some(nx) = hit_iter.peek() {
            if nx.line.saturating_sub(context) > hi + 1 {
                break;
            }
            hi = (nx.line + context).min(nl);
            group.push(hit_iter.next().expect("peeked"));
        }
        if context > 0 && printed_any {
            out.push_str("--\n");
        }
        for n in lo..=hi {
            let hit = group.iter().find(|g| g.line == n);
            let text = render_line(data, &starts, n, hit.map_or(0, |g| g.col));
            out.push_str(&format!("{n}{} {text}\n", if hit.is_some() { ':' } else { '-' }));
        }
        next_line = hi + 1;
        printed_any = true;
    }
}

// Describes skipped files for the footer.
fn skipped_note(results: &[FileResult]) -> String {
    let count = |s| results.iter().filter(|r| r.skip == s).count();
    let parts: Vec<String> =
        [(Skip::Binary, "binary"), (Skip::Large, "large"), (Skip::Unreadable, "unreadable")]
            .iter()
            .filter(|(s, _)| count(*s) > 0)
            .map(|(s, name)| format!("{} {name}", count(*s)))
            .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(", skipped {}", parts.join(" "))
    }
}

// Lists matching files, optionally with counts, capped.
fn render_list(results: &[FileResult], files: &[PathBuf], cwd: &Path, counts: bool) -> String {
    let matched: Vec<&FileResult> = results.iter().filter(|r| r.total > 0).collect();
    let mut out = String::new();
    for r in matched.iter().take(LIST_CAP) {
        let name = display(&files[r.idx], cwd);
        if counts {
            out.push_str(&format!("{name}: {}\n", r.total));
        } else {
            out.push_str(&format!("{name}\n"));
        }
    }
    if matched.len() > LIST_CAP {
        out.push_str(&format!("+{} more files; narrow with a path or -g GLOB\n", matched.len() - LIST_CAP));
    }
    let total: usize = matched.iter().map(|r| r.total).sum();
    out.push_str(&format!(
        "-- {total} matching lines in {} files ({} searched{})",
        matched.len(),
        files.len(),
        skipped_note(results)
    ));
    out
}

// Renders grouped matches up to the cap, with a footer.
fn render_matches(results: &[FileResult], files: &[PathBuf], cwd: &Path, plan: &Plan) -> String {
    let mut out = String::new();
    let (mut shown, mut total, mut matched) = (0, 0, 0);
    for r in results.iter().filter(|r| r.total > 0) {
        total += r.total;
        matched += 1;
        let take = r.hits.len().min(plan.max - shown);
        if take == 0 {
            continue;
        }
        let path = &files[r.idx];
        let Ok(data) = std::fs::read(path) else { continue };
        out.push_str(&format!("{}\n", display(path, cwd)));
        render_file(&mut out, &data, &r.hits[..take], plan.context);
        shown += take;
    }
    out.push_str(&format!(
        "-- {total} matches in {matched} files ({} searched{})",
        files.len(),
        skipped_note(results)
    ));
    if shown < total {
        out.push_str(&format!("; showing {shown}, narrow with a path or -g GLOB, or raise -m"));
    }
    out
}

// Runs the find command.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    let Some(pattern) = p.positional.first() else { return Err(USAGE.into()) };
    let max = p.number("max")?.unwrap_or(DEFAULT_MAX).max(1);
    let context = p.number("context")?.unwrap_or(0);
    let re = build_regex(pattern, p.has("ignore-case"), p.has("fixed"), p.has("word"))?;
    let roots: Vec<PathBuf> = if p.positional.len() > 1 {
        p.positional[1..].iter().map(|a| resolve(a, &ctx.cwd)).collect()
    } else {
        vec![ctx.cwd.clone()]
    };
    let opts = WalkOpts {
        no_ignore: p.has("no-ignore"),
        globs: p.values("glob").iter().map(|s| s.to_string()).collect(),
    };
    let files = walk::collect(&roots, &opts)?;
    let list_mode = p.has("files") || p.has("count");
    let plan = Plan { re, max, context, files_only: p.has("files") };
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let results = search_all(&files, &plan, cpus);
    if !results.iter().any(|r| r.total > 0) {
        return Ok(Output::ok(format!("no matches in {} files{}", files.len(), skipped_note(&results))));
    }
    let text = if list_mode {
        render_list(&results, &files, &ctx.cwd, p.has("count"))
    } else {
        render_matches(&results, &files, &ctx.cwd, &plan)
    };
    Ok(Output::ok(text))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{run_cmd, TempDir};

    // Builds a tree with several matches, a binary file and an ignored file.
    fn tree() -> TempDir {
        let t = TempDir::new();
        t.write("src/a.rs", b"fn alpha() {}\nfn beta() {}\nlet x = alpha();\n");
        t.write("src/b.rs", b"// nothing\nalpha again\n");
        t.write("bin.dat", b"alpha\0binary");
        t.write("skip.log", b"alpha\n");
        t.write(".gitignore", b"*.log\n");
        t
    }

    // Checks the grouped output format and footer.
    #[test]
    fn test_find_groups_by_file() {
        let t = tree();
        let o = run_cmd(t.path(), &["find", "alpha"]);
        assert_eq!(
            o.text,
            "src/a.rs\n1: fn alpha() {}\n3: let x = alpha();\nsrc/b.rs\n2: alpha again\n-- 3 matches in 2 files (4 searched, skipped 1 binary)"
        );
        assert!(o.idable && o.status == 0);
    }

    // Checks context lines, merging and group separators.
    #[test]
    fn test_find_context_merges_and_separates() {
        let t = TempDir::new();
        let body: String = (1..=20)
            .map(|i| if i == 2 || i == 4 || i == 18 { format!("hit{i}\n") } else { format!("l{i}\n") })
            .collect();
        t.write("f.txt", body.as_bytes());
        let o = run_cmd(t.path(), &["find", "hit", "-C", "1"]);
        let lines: Vec<&str> = o.text.lines().collect();
        assert_eq!(lines[..8], ["f.txt", "1- l1", "2: hit2", "3- l3", "4: hit4", "5- l5", "--", "17- l17"]);
        assert_eq!(lines[8], "18: hit18");
    }

    // Checks the no-match message and the no-ignore flag.
    #[test]
    fn test_find_no_matches_and_no_ignore() {
        let t = tree();
        assert_eq!(run_cmd(t.path(), &["find", "zzz"]).text, "no matches in 4 files, skipped 1 binary");
        let o = run_cmd(t.path(), &["find", "alpha", "-u", "-l"]);
        assert!(o.text.contains("skip.log"));
    }

    // Checks fixed-string, word and case-insensitive matching.
    #[test]
    fn test_find_modes() {
        let t = TempDir::new();
        t.write("a.txt", b"a.b\naxb\nfoobar\nfoo bar\nFOO\n");
        let fixed = run_cmd(t.path(), &["find", "-F", "a.b"]).text;
        assert!(fixed.contains("1: a.b") && !fixed.contains("axb"));
        let word = run_cmd(t.path(), &["find", "-w", "foo"]).text;
        assert!(word.contains("4: foo bar") && !word.contains("foobar"));
        let ci = run_cmd(t.path(), &["find", "-i", "foo", "-c"]).text;
        assert!(ci.starts_with("a.txt: 3"));
    }

    // Checks the cap on shown matches and the truncation note.
    #[test]
    fn test_find_cap_and_footer() {
        let t = TempDir::new();
        t.write("a.txt", "x\n".repeat(10).as_bytes());
        t.write("b.txt", "x\n".repeat(10).as_bytes());
        let o = run_cmd(t.path(), &["find", "x", "-m", "12"]);
        assert!(o.text.ends_with(
            "-- 20 matches in 2 files (2 searched); showing 12, narrow with a path or -g GLOB, or raise -m"
        ));
        assert_eq!(o.text.lines().filter(|l| l.contains(": x")).count(), 12);
    }

    // Checks the files and count list modes and their cap.
    #[test]
    fn test_find_list_modes_and_cap() {
        let t = TempDir::new();
        for i in 0..205 {
            t.write(&format!("d/f{i:03}.txt"), b"needle\n");
        }
        let l = run_cmd(t.path(), &["find", "needle", "-l"]).text;
        assert!(l.contains("+5 more files; narrow with a path or -g GLOB"));
        assert!(l.ends_with("-- 205 matching lines in 205 files (205 searched)"));
        let c = run_cmd(t.path(), &["find", "needle", "-c"]).text;
        assert!(c.starts_with("d/f000.txt: 1\n"));
    }

    // Checks that long lines are clipped around the match.
    #[test]
    fn test_find_clips_long_lines() {
        let t = TempDir::new();
        let line = format!("{}NEEDLE{}\n", "a".repeat(500), "b".repeat(500));
        t.write("a.txt", line.as_bytes());
        let o = run_cmd(t.path(), &["find", "NEEDLE"]);
        let l = o.text.lines().nth(1).expect("hit line");
        assert!(l.contains("NEEDLE") && l.contains('…') && l.len() < 300);
    }

    // Checks that a pattern spanning lines reports its first line only.
    #[test]
    fn test_find_multiline_pattern() {
        let t = TempDir::new();
        t.write("a.txt", b"one\ntwo\nthree\nfour\n");
        let o = run_cmd(t.path(), &["find", "two\nthree"]);
        assert!(o.text.contains("2: two") && o.text.contains("-- 1 matches"));
    }

    // Checks the globs, explicit paths and file arguments.
    #[test]
    fn test_find_paths_and_globs() {
        let t = tree();
        let g = run_cmd(t.path(), &["find", "alpha", "-g", "*.rs", "-g", "!b.rs"]).text;
        assert!(g.contains("src/a.rs") && !g.contains("src/b.rs"));
        let p = run_cmd(t.path(), &["find", "alpha", "src/b.rs"]).text;
        assert!(p.starts_with("b.rs") || p.starts_with("src/b.rs"));
    }

    // Checks usage and argument errors.
    #[test]
    fn test_find_errors() {
        let t = tree();
        assert!(run_cmd(t.path(), &["find"]).text.starts_with("error: usage: find PATTERN"));
        assert!(run_cmd(t.path(), &["find", "x", "--bogus"]).text.contains("unknown flag --bogus"));
        assert_eq!(run_cmd(t.path(), &["find", "(", "-m", "1"]).status, 2);
        assert!(run_cmd(t.path(), &["find", "x", "-m", "q"]).text.contains("needs a number"));
        assert!(run_cmd(t.path(), &["find", "x", "nope"]).text.contains("no such file or directory"));
    }

    // Checks regex error messages are one line with a hint for unsupported features.
    #[test]
    fn test_find_regex_error_text() {
        let t = tree();
        let o = run_cmd(t.path(), &["find", "(?=a)b"]);
        assert!(
            o.text.starts_with("error: invalid regex: ")
                && o.text.contains("look-around")
                && !o.text.contains('\n')
        );
        let o = run_cmd(t.path(), &["find", "(a)\\1"]);
        assert!(o.text.contains("backreference") && o.text.contains("-F"));
        let o = run_cmd(t.path(), &["find", "a{2,1}"]);
        assert!(o.text.starts_with("error: invalid regex: ") && !o.text.contains("-F"));
    }

    // Checks large and unreadable files are counted as skipped.
    #[test]
    fn test_find_skips_large_and_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new();
        t.write("big.txt", &vec![b'a'; (super::MAX_FILE + 1) as usize]);
        let p = t.write("locked.txt", b"a\n");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o0)).expect("chmod");
        t.write("ok.txt", b"b\n");
        let o = run_cmd(t.path(), &["find", "zzz"]);
        let readable = std::fs::read(&p).is_ok();
        let want = if readable {
            "no matches in 3 files, skipped 1 large"
        } else {
            "no matches in 3 files, skipped 1 large 1 unreadable"
        };
        assert_eq!(o.text, want);
    }

    // Checks many files exercise the parallel path with stable ordering.
    #[test]
    fn test_find_parallel_ordering() {
        let t = TempDir::new();
        for i in 0..40 {
            t.write(&format!("f{i:02}.txt"), format!("line\nneedle{i}\n").as_bytes());
        }
        let o = run_cmd(t.path(), &["find", "needle"]);
        let heads: Vec<&str> = o.text.lines().filter(|l| l.ends_with(".txt")).collect();
        let mut sorted = heads.clone();
        sorted.sort();
        assert_eq!(heads, sorted);
        assert_eq!(heads.len(), 40);
    }

    // Checks several matches on one line count as one matching line.
    #[test]
    fn test_find_counts_each_line_once() {
        let t = TempDir::new();
        t.write("a.txt", b"aaa bbb aaa\nccc\n");
        let o = run_cmd(t.path(), &["find", "a+"]);
        assert!(o.text.contains("1: aaa bbb aaa") && o.text.contains("-- 1 matches in 1 files"));
    }

    // Checks the per-file cap keeps the total but shows only the cap.
    #[test]
    fn test_find_per_file_cap() {
        let t = TempDir::new();
        t.write("a.txt", "x\n".repeat(10).as_bytes());
        let o = run_cmd(t.path(), &["find", "x", "-m", "3"]).text;
        assert_eq!(o.lines().filter(|l| l.contains(": x")).count(), 3);
        assert!(o.contains("-- 10 matches in 1 files") && o.contains("showing 3"));
    }

    // Checks that once the cap is reached later files are not shown.
    #[test]
    fn test_find_cap_stops_before_later_files() {
        let t = TempDir::new();
        t.write("a.txt", b"x\n");
        t.write("b.txt", b"x\n");
        let o = run_cmd(t.path(), &["find", "x", "-m", "1"]).text;
        assert!(o.contains("a.txt") && !o.contains("b.txt\n") && o.contains("-- 2 matches in 2 files"));
    }

    // Checks search_file classifies a missing path and a directory as unreadable.
    #[test]
    fn test_search_file_unreadable_cases() {
        let t = TempDir::new();
        let plan = Plan {
            re: build_regex("x", false, false, false).expect("regex"),
            max: 5,
            context: 0,
            files_only: false,
        };
        assert_eq!(search_file(0, &t.path().join("gone"), &plan).skip, Skip::Unreadable);
        assert_eq!(search_file(1, t.path(), &plan).skip, Skip::Unreadable);
    }

    // Checks rendering skips a file that vanished after the search.
    #[test]
    fn test_render_matches_skips_vanished_file() {
        let t = TempDir::new();
        let plan = Plan {
            re: build_regex("x", false, false, false).expect("regex"),
            max: 5,
            context: 0,
            files_only: false,
        };
        let results =
            vec![FileResult { idx: 0, total: 1, hits: vec![Hit { line: 1, col: 0 }], skip: Skip::No }];
        let out = render_matches(&results, &[t.path().join("gone")], t.path(), &plan);
        assert_eq!(
            out,
            "-- 1 matches in 1 files (1 searched); showing 0, narrow with a path or -g GLOB, or raise -m"
        );
    }

    // Checks the sequential path gives the same ordered results as the parallel one.
    #[test]
    fn test_search_all_single_cpu() {
        let t = TempDir::new();
        let files: Vec<PathBuf> = (0..12).map(|i| t.write(&format!("f{i:02}.txt"), b"needle\n")).collect();
        let plan = Plan {
            re: build_regex("needle", false, false, false).expect("regex"),
            max: 5,
            context: 0,
            files_only: false,
        };
        let one = search_all(&files, &plan, 1);
        let many = search_all(&files, &plan, 4);
        let idx = |r: &[FileResult]| r.iter().map(|x| x.idx).collect::<Vec<_>>();
        assert_eq!(idx(&one), idx(&many));
        assert_eq!(one.len(), 12);
    }
}
