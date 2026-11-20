// `zazzle flow coverage`: zazzle, not an implementation agent, measures coverage: it runs the coverage checks, reads their report (lcov, Cobertura XML, a Go cover profile, coverage.py JSON, or llvm-cov's missing-lines output), and lists the uncovered lines and partly covered branches of the changed files with the existing tests that exercise them; with an author, a cheap agent writes focused tests for those gaps, kept to test files, that must pass when zazzle measures again, or they are put back.
use super::edit::{self, FILES};
use super::policy::{Check, Policy};
use super::repo;
use super::verify;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Backend, Log};
use std::path::Path;
use std::time::{Duration, Instant};

// The most files and lines of uncovered code an author is shown.
const MAX_FILES: usize = 6;
const MAX_LINES: usize = 40;

// One file's gaps: its path, the lines no test ran, and the branches only partly taken as (line, taken, total).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Gap {
    pub path: String,
    pub lines: Vec<usize>,
    pub branches: Vec<(usize, usize, usize)>,
}

// Adds a file's gaps to the list, merging with an earlier entry for the same file.
fn put(out: &mut Vec<Gap>, g: Gap) {
    if g.lines.is_empty() && g.branches.is_empty() {
        return;
    }
    match out.iter_mut().find(|x| x.path == g.path) {
        Some(x) => {
            x.lines.extend(g.lines);
            x.lines.sort_unstable();
            x.lines.dedup();
            x.branches.extend(g.branches);
        }
        None => out.push(g),
    }
}

// Reads an lcov report: `SF:` files, `DA:line,hits` with zero hits, and `BRDA:line,block,branch,taken` grouped per line.
pub fn parse_lcov(text: &str) -> Vec<Gap> {
    let mut out = Vec::new();
    let mut g = Gap::default();
    let mut br: Vec<(usize, usize, usize)> = Vec::new();
    for l in text.lines().map(str::trim) {
        if let Some(p) = l.strip_prefix("SF:") {
            g = Gap { path: p.to_string(), ..Gap::default() };
            br.clear();
        } else if let Some(d) = l.strip_prefix("DA:") {
            let mut f = d.split(',');
            if let (Some(Ok(n)), Some("0")) = (f.next().map(str::parse::<usize>), f.next()) {
                g.lines.push(n);
            }
        } else if let Some(d) = l.strip_prefix("BRDA:") {
            let f: Vec<&str> = d.split(',').collect();
            let Some(Ok(n)) = f.first().map(|x| x.parse::<usize>()) else { continue };
            let taken = f.get(3).is_some_and(|t| *t != "-" && *t != "0");
            match br.iter_mut().find(|(line, _, _)| *line == n) {
                Some(b) => (b.1, b.2) = (b.1 + usize::from(taken), b.2 + 1),
                None => br.push((n, usize::from(taken), 1)),
            }
        } else if l == "end_of_record" {
            g.branches = br.iter().copied().filter(|(_, t, n)| t < n).collect();
            put(&mut out, std::mem::take(&mut g));
        }
    }
    out
}

// The value of `name="..."` in an XML tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let i = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    tag[i..].split('"').next()
}

// Reads a Cobertura XML report: each class's file, its lines with zero hits, and lines whose condition coverage is below 100%.
pub fn parse_cobertura(text: &str) -> Vec<Gap> {
    let mut out = Vec::new();
    let mut g = Gap::default();
    for tag in text.split('<').map(|t| t.split('>').next().unwrap_or_default()) {
        if tag.starts_with("class ") {
            put(&mut out, std::mem::take(&mut g));
            g.path = attr(tag, "filename").unwrap_or_default().to_string();
        } else if tag.starts_with("line ") {
            let Some(n) = attr(tag, "number").and_then(|n| n.parse::<usize>().ok()) else { continue };
            if attr(tag, "hits") == Some("0") {
                g.lines.push(n);
            }
            let cond = attr(tag, "condition-coverage")
                .and_then(|c| c.split_once('('))
                .and_then(|(_, r)| r.trim_end_matches(')').split_once('/'));
            if let Some((t, all)) =
                cond.and_then(|(t, a)| Some((t.parse::<usize>().ok()?, a.parse::<usize>().ok()?)))
            {
                if t < all && !g.branches.iter().any(|b| b.0 == n) {
                    g.branches.push((n, t, all));
                }
            }
        }
    }
    put(&mut out, g);
    out
}

// Reads a Go cover profile: `file:startLine.col,endLine.col statements count` blocks never run.
pub fn parse_go(text: &str) -> Vec<Gap> {
    let mut out = Vec::new();
    for l in text.lines().filter(|l| !l.starts_with("mode:")) {
        let Some((file, rest)) = l.rsplit_once(':') else { continue };
        let f: Vec<&str> = rest.split_whitespace().collect();
        let (Some(span), Some(&"0")) = (f.first(), f.get(2)) else { continue };
        let line = |s: &str| s.split('.').next().and_then(|n| n.parse::<usize>().ok());
        if let Some((a, b)) = span.split_once(',').and_then(|(a, b)| Some((line(a)?, line(b)?))) {
            put(&mut out, Gap { path: file.to_string(), lines: (a..=b).collect(), branches: Vec::new() });
        }
    }
    out
}

// Reads coverage.py's JSON report: each file's missing lines and missing branches (counted per source line).
pub fn parse_coveragepy(text: &str) -> Vec<Gap> {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    let mut out = Vec::new();
    for (path, f) in v["files"].as_object().into_iter().flatten() {
        let lines = f["missing_lines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| n.as_u64().map(|n| n as usize))
            .collect();
        let mut branches: Vec<(usize, usize, usize)> = Vec::new();
        for b in f["missing_branches"].as_array().into_iter().flatten() {
            let Some(n) = b[0].as_u64().map(|n| n as usize) else { continue };
            match branches.iter_mut().find(|x| x.0 == n) {
                Some(x) => x.2 += 1,
                None => branches.push((n, 1, 2)),
            }
        }
        put(&mut out, Gap { path: path.clone(), lines, branches });
    }
    out
}

// Reads llvm-cov's `--show-missing-lines` output: `path: 12-15, 20` lines.
pub fn parse_missing(text: &str) -> Vec<Gap> {
    let mut out = Vec::new();
    for l in text.lines() {
        let Some((path, list)) = l.rsplit_once(": ") else { continue };
        if path.contains(char::is_whitespace) || !path.contains('.') {
            continue;
        }
        let mut lines = Vec::new();
        for part in list.split(',').map(str::trim) {
            let (a, b) = part.split_once('-').unwrap_or((part, part));
            match (a.parse::<usize>(), b.parse::<usize>()) {
                (Ok(a), Ok(b)) if a <= b => lines.extend(a..=b),
                _ => {
                    lines.clear();
                    break;
                }
            }
        }
        put(&mut out, Gap { path: path.to_string(), lines, branches: Vec::new() });
    }
    out
}

// Reads a report by its content: lcov, Cobertura, a Go profile, coverage.py JSON, else missing-lines text.
pub fn parse(text: &str) -> Vec<Gap> {
    let t = text.trim_start();
    if t.starts_with("TN:") || t.starts_with("SF:") {
        parse_lcov(text)
    } else if t.starts_with("<?xml") || t.starts_with("<coverage") {
        parse_cobertura(text)
    } else if t.starts_with("mode:") {
        parse_go(text)
    } else if t.starts_with('{') {
        parse_coveragepy(text)
    } else {
        parse_missing(text)
    }
}

// Lines as ranges: `12-15, 20`.
pub fn ranges(lines: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        while j + 1 < lines.len() && lines[j + 1] == lines[j] + 1 {
            j += 1;
        }
        out.push(if i == j { lines[i].to_string() } else { format!("{}-{}", lines[i], lines[j]) });
        i = j + 1;
    }
    out.join(", ")
}

// Maps a report's path to the repository path it names among the candidates: the same path, the same path under the top, or the candidate it ends with.
fn resolve(top: &Path, path: &str, candidates: &[String]) -> Option<String> {
    let rel = path.strip_prefix(&format!("{}/", top.display())).unwrap_or(path).trim_start_matches("./");
    candidates
        .iter()
        .find(|c| c.as_str() == rel || rel.ends_with(&format!("/{c}")) || c.ends_with(&format!("/{rel}")))
        .cloned()
}

// The existing test files that mention a file's name, at most three: the fixtures a test author should model on.
pub fn fixtures(top: &Path, path: &str) -> Vec<String> {
    let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let found =
        repo::git(top, &["grep", "-l", "-F", &stem, "--", "*test*", "*Test*", "*spec*"]).unwrap_or_default();
    found.lines().filter(|f| *f != path).take(3).map(str::to_string).collect()
}

// Runs the coverage checks and reads their gaps in `scope` (the changed files; every file without a scope): the gaps, a report, and whether every check passed.
pub fn measure(
    top: &Path,
    p: &Policy,
    scope: Option<&[String]>,
    limit: Duration,
) -> Result<(Vec<Gap>, String, bool), String> {
    let checks: Vec<&Check> = p
        .checks
        .iter()
        .filter(|c| c.kind == "coverage")
        .filter(|c| scope.is_none_or(|s| c.paths.is_empty() || s.iter().any(|x| repo::under(x, &c.paths))))
        .collect();
    if checks.is_empty() {
        return Err("no coverage check applies: add one with kind \"coverage\" (and \"report\" naming its lcov, Cobertura, Go or coverage.py output) to .zazzle/flow.json".into());
    }
    let logs = repo::state(top, "logs")?;
    let (mut gaps, mut out, mut ok) = (Vec::new(), String::new(), true);
    for c in checks {
        let safe: String = c.name.chars().map(|ch| if ch.is_alphanumeric() { ch } else { '-' }).collect();
        let log = logs.join(format!("coverage-{safe}.log"));
        let o = verify::run_check(top, c, &log, limit);
        ok &= o.status == "passed";
        let text = match &c.report {
            Some(r) => std::fs::read_to_string(top.join(&c.dir).join(r)).unwrap_or_default(),
            None => std::fs::read_to_string(&log).unwrap_or_default(),
        };
        let found = parse(&text);
        out.push_str(&format!(
            "  {} {}: {}; {} file(s) with gaps in its report\n",
            if o.status == "passed" { "✓" } else { "✗" },
            c.name,
            o.summary(),
            found.len()
        ));
        for g in found {
            put(&mut gaps, g);
        }
    }
    let all: Vec<String> = match scope {
        Some(s) => s.to_vec(),
        None => gaps.iter().map(|g| g.path.clone()).collect(),
    };
    let gaps =
        gaps.into_iter().filter_map(|g| Some(Gap { path: resolve(top, &g.path, &all)?, ..g })).collect();
    Ok((gaps, out, ok))
}

// The gaps as report lines, with their fixtures.
pub fn listing(top: &Path, gaps: &[Gap]) -> String {
    let mut out = String::new();
    if gaps.is_empty() {
        out.push_str("  no uncovered lines or partial branches in these files\n");
    }
    for g in gaps {
        let br: Vec<String> = g.branches.iter().map(|(l, t, n)| format!("{l} ({t} of {n})")).collect();
        out.push_str(&format!(
            "  {}: uncovered lines {}{}\n",
            g.path,
            if g.lines.is_empty() { "none".to_string() } else { ranges(&g.lines) },
            if br.is_empty() { String::new() } else { format!("; partial branches at {}", br.join(", ")) }
        ));
        let fx = fixtures(top, &g.path);
        if !fx.is_empty() {
            out.push_str(&format!("    tests to model on: {}\n", fx.join(", ")));
        }
    }
    out
}

// The uncovered source lines of the gaps with a line around each, for an author's prompt.
fn excerpts(top: &Path, gaps: &[Gap]) -> String {
    let mut out = String::new();
    for g in gaps.iter().take(MAX_FILES) {
        let text = std::fs::read_to_string(top.join(&g.path)).unwrap_or_default();
        let src: Vec<&str> = text.lines().collect();
        let mut want: Vec<usize> = g.lines.iter().chain(g.branches.iter().map(|b| &b.0)).copied().collect();
        want.sort_unstable();
        want.dedup();
        out.push_str(&format!("{}:\n", g.path));
        for n in want.into_iter().take(MAX_LINES) {
            if let Some(l) = src.get(n.saturating_sub(1)) {
                out.push_str(&format!("{n}\t{l}\n"));
            }
        }
    }
    out
}

// Measures the change's coverage gaps and, with an author, has it write tests for them inside `tests`, keeping them only if every coverage check still passes; returns the report and whether it ended with the checks passing.
#[allow(clippy::too_many_arguments)]
pub fn run(
    top: &Path,
    p: &Policy,
    base: Option<&str>,
    author: Option<&Rung>,
    tests: &[String],
    b: &dyn Backend,
    log: &Log,
    limit: Duration,
) -> Result<(String, bool), String> {
    let scope = match base {
        Some(bb) => Some(
            repo::changed(top, bb, &repo::worktree_tree(top)?)?
                .into_iter()
                .filter(|c| !p.is_doc(c))
                .collect::<Vec<_>>(),
        ),
        None => None,
    };
    let (gaps, checks, ok) = measure(top, p, scope.as_deref(), limit)?;
    let mut out = format!("coverage, measured by zazzle:\n{checks}{}", listing(top, &gaps));
    let Some(r) = author.filter(|_| !gaps.is_empty() && ok) else {
        out.push_str(match (ok, gaps.is_empty()) {
            (false, _) => "-- a coverage check failed; fix that before closing gaps\n",
            (true, true) => "-- no gaps in the changed files\n",
            (true, false) => "-- gaps listed; pass --author AGENT --tests PATH to have them closed\n",
        });
        return Ok((out, ok));
    };
    let before = repo::worktree_tree(top)?;
    let fx: Vec<String> = gaps.iter().flat_map(|g| fixtures(top, &g.path)).take(1).collect();
    let model: String = fx
        .iter()
        .map(|f| {
            format!(
                "Existing test to model on ({f}):\n```\n{}```\n",
                std::fs::read_to_string(top.join(f))
                    .unwrap_or_default()
                    .lines()
                    .take(80)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
        .collect();
    let code = excerpts(top, &gaps);
    let prompt = format!(
        "Write focused tests that run these uncovered lines and branches. Assert what the code does now; if a line's behavior looks wrong, do not encode it: say so in a GAP line instead. Do not change production code, and do not run the tests or measure coverage yourself: zazzle does that after you. Write only inside: {}.\n{code}{model}{}",
        if tests.is_empty() { "test files".to_string() } else { tests.join(", ") },
        if r.hosted() { "Edit the files, then reply with STATUS, GAP and CONFIDENCE lines." } else { FILES }
    );
    let in_tests =
        |x: &str| if tests.is_empty() { x.to_lowercase().contains("test") } else { repo::under(x, tests) };
    let sections = [("evidence", code.as_str()), ("reference", model.as_str())];
    let e =
        edit::edit(top, b, log, r, "coverage tests", &prompt, &sections, &in_tests, Instant::now() + limit)?;
    if !e.reverted.is_empty() {
        out.push_str(&format!(
            "{} changed paths outside the test files, put back: {}\n",
            r.label(),
            e.reverted.join(", ")
        ));
    }
    for g in crate::investigate::cite::parse_answer(&e.reply).map(|a| a.unknowns).unwrap_or_default() {
        out.push_str(&format!("the author doubts: {g}\n"));
    }
    if e.changed.is_empty() {
        out.push_str(&format!("-- {} wrote no tests\n", r.label()));
        return Ok((out, false));
    }
    let (after, checks, ok) = measure(top, p, scope.as_deref(), limit)?;
    if !ok {
        repo::restore(top, &before, &e.changed)?;
        out.push_str(&format!(
            "after {}:\n{checks}-- a coverage check failed with the new tests, so they were put back\n",
            r.label()
        ));
        return Ok((out, false));
    }
    let count = |gs: &[Gap]| gs.iter().map(|g| g.lines.len() + g.branches.len()).sum::<usize>();
    out.push_str(&format!("after {} wrote {}:\n{}", r.label(), e.changed.join(", "), listing(top, &after)));
    out.push_str(&format!(
        "-- gaps {} → {}; the new tests pass; check their assertions before you keep them\n",
        count(&gaps),
        count(&after)
    ));
    Ok((out, true))
}
