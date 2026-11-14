// The clean command: shrinks piped command output for agents, marking anything it drops.
use super::read::fit;
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::tokens;
use crate::util::clip;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

const SPEC: &[Flag] = &[opt("as", '\0'), opt("budget", 'b'), flag("lossless", '\0'), flag("stats", '\0')];
const USAGE: &str =
    "usage: CMD | zazzle clean [--as auto|grep|json|text] [--lossless] [--budget T] [--stats]";
const MAX_INPUT: usize = 64 << 20;
const DEFAULT_BUDGET: usize = 8000;
const LONG_LINE: usize = 600;
const CLIP_TO: usize = 300;
const MIN_BLOB: usize = 200;
const LOSSY: [&str; 4] = ["progress", "blobs", "long-lines", "budget"];
const BAR: &str = "█▉▊▋▌▍▎▏░▒▓■□#=";

// How the input should be read.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Kind {
    Auto,
    Grep,
    Json,
    Text,
}

// One parsed grep line: its file, optional line number, whether it matched, and its text.
#[derive(Debug, Clone, Copy)]
struct Hit<'a> {
    path: &'a str,
    line: Option<&'a str>,
    is_match: bool,
    text: &'a str,
}

// The cleaned lines and the names of the steps that changed them.
struct Cleaned {
    lines: Vec<String>,
    steps: Vec<&'static str>,
}

// Parses the --as value.
fn kind(v: Option<&str>) -> Result<Kind, String> {
    match v {
        None | Some("auto") => Ok(Kind::Auto),
        Some("grep") => Ok(Kind::Grep),
        Some("json") => Ok(Kind::Json),
        Some("text") => Ok(Kind::Text),
        Some(other) => Err(format!("--as must be auto, grep, json or text, not '{other}'")),
    }
}

// Removes terminal escape sequences: colours, cursor moves and window titles.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                for c in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        it.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            Some(' '..='/') => {
                while it.next_if(|c| (' '..='/').contains(c)).is_some() {}
                it.next();
            }
            _ => {}
        }
    }
    out
}

// Keeps what a terminal shows for a line redrawn with carriage returns: the last non-empty frame.
fn last_frame(line: &str) -> &str {
    line.rsplit('\r').find(|s| !s.is_empty()).unwrap_or("")
}

// Removes whitespace outside strings from valid JSON, keeping key order and number spelling.
fn minify(doc: &str) -> Option<String> {
    let t = doc.trim();
    if !(t.starts_with('{') || t.starts_with('[')) || serde_json::from_str::<serde_json::Value>(t).is_err() {
        return None;
    }
    let mut out = String::with_capacity(t.len());
    let (mut in_str, mut esc) = (false, false);
    for c in t.chars() {
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if !c.is_ascii_whitespace() {
            out.push(c);
        }
    }
    Some(out)
}

// Minifies a JSON document, or JSON Lines where every non-blank line is an object or array.
fn minify_json(text: &str) -> Option<Vec<String>> {
    if let Some(doc) = minify(text) {
        return Some(vec![doc]);
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return None;
    }
    lines.into_iter().map(minify).collect()
}

// Trims trailing whitespace, resolves carriage-return redraws and squeezes runs of blank lines.
fn tidy(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = last_frame(raw).trim_end();
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line.to_string());
    }
    if out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

// Matches a progress-bar line: a percentage next to a bar, or a bracketed bar.
fn progress_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let bar = format!("[{}]{{5}}", regex::escape(BAR));
        Regex::new(&format!(r"\d%.*{bar}|{bar}.*\d%|\[[ .\-]*[=#>]{{2,}}[=#>\-. ]*\]")).expect("valid regex")
    })
}

// Replaces each run of progress-bar lines with its last line and a count of the rest.
fn drop_progress(lines: Vec<String>) -> (Vec<String>, bool) {
    let re = progress_re();
    let (mut out, mut run, mut changed) = (Vec::with_capacity(lines.len()), Vec::new(), false);
    for line in lines.into_iter().chain(std::iter::once(String::new())) {
        if re.is_match(&line) {
            run.push(line);
            continue;
        }
        if run.len() > 1 {
            out.push(format!("… {} progress lines dropped", run.len() - 1));
            changed = true;
        }
        out.extend(run.pop());
        run.clear();
        out.push(line);
    }
    out.pop();
    (out, changed)
}

// True for text that looks like a file path: no spaces, a slash or dot, and not just a number.
fn plausible_path(p: &str) -> bool {
    !p.contains(char::is_whitespace)
        && (p.contains('/') || p.contains('.'))
        && !p.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

// True for a non-empty run of ASCII digits.
fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

// Reads a `path:N:text` match line, or `path:text` when the output has no line numbers.
fn match_line(line: &str, numbered: bool) -> Option<Hit<'_>> {
    let (path, rest) = line.split_once(':')?;
    if !plausible_path(path) {
        return None;
    }
    if !numbered {
        return Some(Hit { path, line: None, is_match: true, text: rest });
    }
    let (n, text) = rest.split_once(':')?;
    is_number(n).then_some(Hit { path, line: Some(n), is_match: true, text })
}

// Reads a `path-N-text` context line for a path seen on a match line.
fn context_line<'a>(line: &'a str, path: &'a str) -> Option<Hit<'a>> {
    let (n, text) = line.strip_prefix(path)?.strip_prefix('-')?.split_once('-')?;
    is_number(n).then_some(Hit { path, line: Some(n), is_match: false, text })
}

// Parses every line as grep or rg output; None if any line is something else.
fn parse_grep(lines: &[String]) -> Option<Vec<Hit<'_>>> {
    let numbered = lines.iter().any(|l| match_line(l, true).is_some());
    let matches: Vec<Option<Hit>> = lines.iter().map(|l| match_line(l, numbered)).collect();
    let mut next = vec![None; lines.len()];
    let mut seen = None;
    for i in (0..lines.len()).rev() {
        seen = matches[i].map(|h| h.path).or(seen);
        next[i] = seen;
    }
    let (mut hits, mut prev) = (Vec::with_capacity(lines.len()), None);
    for (i, l) in lines.iter().enumerate() {
        if l == "--" || l.is_empty() {
            continue;
        }
        let hit = matches[i].or_else(|| {
            numbered.then(|| [prev, next[i]].into_iter().flatten().find_map(|p| context_line(l, p))).flatten()
        })?;
        prev = Some(hit.path);
        hits.push(hit);
    }
    (!hits.is_empty()).then_some(hits)
}

// Regroups grep or rg output by file in zazzle find's layout: a path line, then `N:` matches and `N-` context.
fn group_grep(lines: &[String]) -> Option<Vec<String>> {
    let hits = parse_grep(lines)?;
    let (mut order, mut groups): (Vec<&str>, HashMap<&str, Vec<String>>) = (Vec::new(), HashMap::new());
    for h in hits {
        let rows = groups.entry(h.path).or_insert_with(|| {
            order.push(h.path);
            Vec::new()
        });
        rows.push(match h.line {
            Some(n) => format!("{n}{} {}", if h.is_match { ':' } else { '-' }, h.text).trim_end().to_string(),
            None => format!("  {}", h.text),
        });
    }
    let mut out = Vec::new();
    for path in order {
        out.push(path.to_string());
        out.append(groups.get_mut(path).expect("grouped path"));
    }
    Some(out)
}

// Collapses runs of identical lines into one line and a count, when that is shorter.
fn collapse_repeats(lines: Vec<String>) -> (Vec<String>, bool) {
    let (mut out, mut changed) = (Vec::with_capacity(lines.len()), false);
    let mut it = lines.into_iter().peekable();
    while let Some(line) = it.next() {
        let mut extra = 0;
        while it.next_if_eq(&line).is_some() {
            extra += 1;
        }
        let note = format!("… previous line repeated {extra} more times");
        let collapse = extra > 0 && note.len() < (line.len() + 1) * extra;
        out.push(line.clone());
        if collapse {
            out.push(note);
            changed = true;
        } else {
            out.extend(std::iter::repeat_n(line, extra));
        }
    }
    (out, changed)
}

// Matches long base64 or hex runs such as embedded images, keys and dumps.
fn blob_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!("[A-Za-z0-9+/_-]{{{MIN_BLOB},}}={{0,2}}")).expect("valid regex"))
}

// Replaces long encoded blobs with a stub giving their length.
fn replace_blobs(lines: Vec<String>) -> (Vec<String>, bool) {
    let re = blob_re();
    let mut changed = false;
    let out = lines
        .into_iter()
        .map(|l| {
            let r = re.replace_all(&l, |c: &regex::Captures| {
                let m = &c[0];
                if m.bytes().any(|b| b.is_ascii_digit()) && m.bytes().any(|b| b.is_ascii_alphabetic()) {
                    format!("<blob {} chars>", m.len())
                } else {
                    m.to_string()
                }
            });
            changed |= r.len() != l.len();
            r.into_owned()
        })
        .collect();
    (out, changed)
}

// Cuts a line to about max bytes and says how long it was.
fn clip_marked(line: &str, max: usize) -> String {
    format!("{} [line was {} bytes]", clip(line, 0, max), line.len())
}

// Clips lines too long to be useful, such as minified code or dumps.
fn clip_long(lines: Vec<String>) -> (Vec<String>, bool) {
    let changed = lines.iter().any(|l| l.len() > LONG_LINE);
    (
        lines.into_iter().map(|l| if l.len() > LONG_LINE { clip_marked(&l, CLIP_TO) } else { l }).collect(),
        changed,
    )
}

// Keeps the head and tail within a token budget and says what was cut from the middle.
fn fit_budget(lines: Vec<String>, budget: usize) -> (Vec<String>, bool) {
    let max_line = budget * 7 / 4;
    let changed = lines.iter().any(|l| l.len() > max_line);
    let mut lines: Vec<String> =
        lines.into_iter().map(|l| if l.len() > max_line { clip_marked(&l, max_line) } else { l }).collect();
    let costs: Vec<usize> = lines.iter().map(|l| tokens::from_bytes(l.len() + 1)).collect();
    let (head, tail) = fit(&costs, budget);
    if head == tail {
        return (lines, changed);
    }
    let cut: usize = costs[head..tail].iter().sum();
    let note = format!(
        "… {} lines (~{cut} tokens) cut to fit the budget of {budget}; narrow the command or raise --budget",
        tail - head
    );
    lines.splice(head..tail, [note]);
    (lines, true)
}

// Records a step's name when it changed the text.
fn step(
    steps: &mut Vec<&'static str>,
    name: &'static str,
    (lines, changed): (Vec<String>, bool),
) -> Vec<String> {
    if changed {
        steps.push(name);
    }
    lines
}

// Applies the line steps to text that is not JSON.
fn clean_text(plain: &str, kind: Kind, lossy: bool, steps: &mut Vec<&'static str>) -> Vec<String> {
    let mut lines = tidy(plain);
    if lines.iter().map(|l| l.len() + 1).sum::<usize>() < plain.len() {
        steps.push("whitespace");
    }
    if lossy {
        lines = step(steps, "progress", drop_progress(lines));
    }
    let bytes = |v: &[String]| v.iter().map(|l| l.len() + 1).sum::<usize>();
    if kind != Kind::Text {
        if let Some(g) = group_grep(&lines).filter(|g| bytes(g) < bytes(&lines)) {
            lines = step(steps, "grep", (g, true));
        }
    }
    step(steps, "repeats", collapse_repeats(lines))
}

// Runs every step over the input; never returns more text than came in before the budget.
fn clean(input: &str, kind: Kind, lossy: bool, budget: usize) -> Cleaned {
    let mut steps = Vec::new();
    let plain = if input.contains('\x1b') {
        steps.push("ansi");
        strip_ansi(input)
    } else {
        input.to_string()
    };
    let json = if matches!(kind, Kind::Auto | Kind::Json) { minify_json(&plain) } else { None };
    let is_json = json.is_some();
    let mut lines = match json {
        Some(j) => step(&mut steps, "json", (j, true)),
        None => clean_text(&plain, kind, lossy, &mut steps),
    };
    if lossy {
        lines = step(&mut steps, "blobs", replace_blobs(lines));
        if !is_json {
            lines = step(&mut steps, "long-lines", clip_long(lines));
        }
    }
    if lines.iter().map(|l| l.len() + 1).sum::<usize>() > input.len() + 1 {
        steps.clear();
        lines = input.lines().map(str::to_string).collect();
    }
    let lines = step(&mut steps, "budget", fit_budget(lines, budget));
    Cleaned { lines, steps }
}

// Strips terminal escapes and redraws and tidies whitespace, leaving every line's content as it is.
pub fn tidy_lines(input: &str) -> Vec<String> {
    if input.contains('\x1b') {
        tidy(&strip_ansi(input))
    } else {
        tidy(input)
    }
}

// True when the cleaning dropped content that read-again --full can bring back.
pub fn cuts(steps: &[&str]) -> bool {
    steps.iter().any(|s| LOSSY.contains(s))
}

// Stores an output and its full input for read-again, tagging it with the id and, when content was cut, how to get it all.
pub fn store(dir: &std::path::Path, shown: String, full: &str, cut: bool) -> String {
    let id = crate::outputs::id_of(&shown, full);
    let hint = if cut { format!("\n[full output: read-again r:{id} --full]") } else { String::new() };
    match crate::outputs::save(dir, &id, &format!("{shown}{hint}"), full, crate::outputs::limit()) {
        Ok(()) => format!("[r:{id}]\n{shown}{hint}"),
        Err(e) => format!("{shown}\n[output not stored for read-again: {e}]"),
    }
}

// Splits text into lines with terminal escapes and redraws removed, keeping every line so numbers match the original.
pub fn plain_lines(input: &str) -> Vec<String> {
    let text = if input.contains('\x1b') { strip_ansi(input) } else { input.to_string() };
    text.lines().map(|l| last_frame(l).trim_end().to_string()).collect()
}

// Cleans one block of text and returns which steps changed it.
pub fn shrink_steps(input: &str, lossy: bool, budget: usize) -> (String, Vec<&'static str>) {
    let c = clean(input, Kind::Auto, lossy, budget);
    (c.lines.join("\n"), c.steps)
}

// Cleans one block of text with detection on, for callers other than the command line such as the MCP proxy.
pub fn shrink(input: &str, lossy: bool, budget: usize) -> String {
    clean(input, Kind::Auto, lossy, budget).lines.join("\n")
}

// Reads standard input, cleans it and reports what changed if asked.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    if !p.positional.is_empty() {
        return Err(format!("clean reads standard input only; {USAGE}"));
    }
    let kind = kind(p.value("as"))?;
    let budget = p.number("budget")?.unwrap_or(DEFAULT_BUDGET);
    if budget == 0 {
        return Err("--budget must be at least 1".into());
    }
    let (bytes, cut) = ctx.read_stdin_max(MAX_INPUT)?;
    let input = String::from_utf8_lossy(&bytes);
    let c = clean(&input, kind, !p.has("lossless"), budget);
    let mut text = if c.lines.is_empty() { "(empty)".to_string() } else { c.lines.join("\n") };
    if cut {
        text.push_str(&format!("\n… input cut at {} MiB", MAX_INPUT >> 20));
    }
    if p.has("stats") {
        let steps = if c.steps.is_empty() { "none".to_string() } else { c.steps.join(",") };
        let (before, after) = (tokens::estimate(&input), tokens::estimate(&text));
        text.push_str(&format!("\n[clean ~{before}→~{after} tokens; steps: {steps}]"));
    }
    Ok(Output::plain(match &ctx.engine.outputs {
        Some(dir) => store(dir, text, &input, cut || cuts(&c.steps)),
        None => text,
    }))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::cell::RefCell;
    use std::path::PathBuf;

    // Runs clean with the given arguments over the given standard input.
    fn go(args: &[&str], input: &str) -> Output {
        let e = Engine::new();
        let ctx = Ctx {
            engine: &e,
            cwd: PathBuf::from("/"),
            session: String::new(),
            stdin: RefCell::new(Some(Box::new(std::io::Cursor::new(input.as_bytes().to_vec())))),
            caller: RefCell::new(None),
        };
        let mut all = vec!["clean".to_string()];
        all.extend(args.iter().map(|s| s.to_string()));
        super::super::run(&ctx, &all)
    }

    // Converts string slices into owned lines.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks the --as values and the error for an unknown one.
    #[test]
    fn test_kind_values() {
        assert_eq!(kind(None), Ok(Kind::Auto));
        assert_eq!(kind(Some("auto")), Ok(Kind::Auto));
        assert_eq!(kind(Some("grep")), Ok(Kind::Grep));
        assert_eq!(kind(Some("json")), Ok(Kind::Json));
        assert_eq!(kind(Some("text")), Ok(Kind::Text));
        assert_eq!(kind(Some("xml")).unwrap_err(), "--as must be auto, grep, json or text, not 'xml'");
    }

    // Checks colours, cursor moves and both title terminators are removed and other text kept.
    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[1;31merror\x1b[0m: x"), "error: x");
        assert_eq!(strip_ansi("\x1b]0;title\x07a\x1b]0;t\x1b\\b"), "ab");
        assert_eq!(strip_ansi("\x1b]0;t\x1bzc"), "zc");
        assert_eq!(strip_ansi("a\x1b(Bb\x1b"), "ab");
        assert_eq!(strip_ansi("\x1b$(Bx"), "x");
        assert_eq!(strip_ansi("\x1b[2"), "");
        assert_eq!(strip_ansi("\x1b]unterminated"), "");
    }

    // Checks carriage-return redraws keep the last visible frame.
    #[test]
    fn test_last_frame() {
        assert_eq!(last_frame("10%\r50%\r100%"), "100%");
        assert_eq!(last_frame("done\r"), "done");
        assert_eq!(last_frame("plain"), "plain");
        assert_eq!(last_frame("\r"), "");
    }

    // Checks JSON whitespace goes, strings and key order stay, and non-JSON is refused.
    #[test]
    fn test_minify() {
        assert_eq!(
            minify("{\n  \"b\": 1,\n  \"a\": \"x  \\\" y\"\n}\n").as_deref(),
            Some(r#"{"b":1,"a":"x  \" y"}"#)
        );
        assert_eq!(minify(" [1, 2.50, 1e400] ").as_deref(), None);
        assert_eq!(minify(" [1, 2.50] ").as_deref(), Some("[1,2.50]"));
        assert_eq!(minify("{\"a\": \"\\\\\"}").as_deref(), Some(r#"{"a":"\\"}"#));
        assert_eq!(minify("hello"), None);
        assert_eq!(minify("{broken"), None);
    }

    // Checks documents and JSON Lines are minified and mixed text is not.
    #[test]
    fn test_minify_json() {
        assert_eq!(minify_json("{ \"a\": 1 }"), Some(v(&["{\"a\":1}"])));
        assert_eq!(minify_json("{ \"a\": 1 }\n\n[ 2 ]\n"), Some(v(&["{\"a\":1}", "[2]"])));
        assert_eq!(minify_json("{ \"a\": 1 }\nnot json\n"), None);
        assert_eq!(minify_json("{ \"a\": "), None);
    }

    // Checks trailing spaces, redraws and blank runs are tidied.
    #[test]
    fn test_tidy() {
        assert_eq!(tidy("\n\na  \r\n\n\n\nb\r50%\rc\n\n"), v(&["a", "", "c"]));
        assert_eq!(tidy(""), Vec::<String>::new());
    }

    // Checks runs of progress bars shrink to their last line and ordinary lines stay.
    #[test]
    fn test_drop_progress() {
        let lines = v(&["start", " 10% |██████    |", " 60% |████████  |", "100% |██████████|", "done"]);
        let (out, changed) = drop_progress(lines);
        assert!(changed);
        assert_eq!(out, v(&["start", "… 2 progress lines dropped", "100% |██████████|", "done"]));
        let (out, changed) = drop_progress(v(&["Building [====>    ] 3/9", "ok 100%", "a = b"]));
        assert!(!changed);
        assert_eq!(out, v(&["Building [====>    ] 3/9", "ok 100%", "a = b"]));
        assert!(progress_re().is_match("[#####.....] 50%"));
        assert!(progress_re().is_match("50% ===== "));
        assert!(!progress_re().is_match("[ ] task"));
    }

    // Checks plausible paths and the numbers that are not.
    #[test]
    fn test_plausible_path() {
        assert!(plausible_path("src/a.rs") && plausible_path("a.txt") && plausible_path("dir/x"));
        assert!(!plausible_path("12") && !plausible_path("1.5") && !plausible_path("Error"));
        assert!(!plausible_path("my file.rs"));
        assert!(is_number("42") && !is_number("") && !is_number("4a"));
    }

    // Checks match and context lines parse, and other lines are refused.
    #[test]
    fn test_match_and_context_lines() {
        let h = match_line("src/a.rs:12:let x: u8 = 1;", true).expect("hit");
        assert_eq!((h.path, h.line, h.is_match, h.text), ("src/a.rs", Some("12"), true, "let x: u8 = 1;"));
        let h = match_line("src/a.rs:fn main()", false).expect("hit");
        assert_eq!((h.line, h.text), (None, "fn main()"));
        assert!(match_line("src/a.rs:fn main()", true).is_none());
        assert!(match_line("src/a.rs-12-ctx", true).is_none());
        assert!(match_line("12:30:01 started", true).is_none());
        assert!(match_line("no colon here", true).is_none());
        let h = context_line("src/my-file.rs-13-  ctx-y", "src/my-file.rs").expect("context");
        assert_eq!((h.line, h.is_match, h.text), (Some("13"), false, "  ctx-y"));
        assert!(context_line("src/a.rs-x-y", "src/a.rs").is_none());
        assert!(context_line("other.rs-1-y", "src/a.rs").is_none());
    }

    // Checks numbered output with context and separators is regrouped by file.
    #[test]
    fn test_group_grep_numbered() {
        let lines = v(&[
            "src/a.rs-1-use x;",
            "src/a.rs-2-",
            "src/a.rs:3:fn foo() {}",
            "--",
            "src/a.rs:9:foo();",
            "src/b.rs:3:foo()",
            "src/b.rs-4-}",
            "src/a.rs:20:foo",
        ]);
        assert_eq!(
            group_grep(&lines).expect("grep"),
            v(&[
                "src/a.rs",
                "1- use x;",
                "2-",
                "3: fn foo() {}",
                "9: foo();",
                "20: foo",
                "src/b.rs",
                "3: foo()",
                "4- }"
            ])
        );
    }

    // Checks unnumbered output groups with indented text, and anything else is refused.
    #[test]
    fn test_group_grep_other_forms() {
        assert_eq!(
            group_grep(&v(&["a.txt:one", "a.txt:two", "b.txt:three"])).expect("grep"),
            v(&["a.txt", "  one", "  two", "b.txt", "  three"])
        );
        assert!(group_grep(&v(&["src/a.rs:1:x", "Binary file b matches"])).is_none());
        assert!(group_grep(&v(&["just text", "more"])).is_none());
        assert!(group_grep(&v(&["--", ""])).is_none());
        assert!(group_grep(&v(&["a.txt:one", "a.txt-2-ctx"])).is_none());
    }

    // Checks repeats collapse only when the note is shorter.
    #[test]
    fn test_collapse_repeats() {
        let long = "error: connection refused by peer";
        let (out, changed) = collapse_repeats(v(&["a", long, long, long, "b", "b"]));
        assert!(changed);
        assert_eq!(out, v(&["a", long, "… previous line repeated 2 more times", "b", "b"]));
        let (out, changed) = collapse_repeats(v(&["x", "x", "y"]));
        assert!(!changed);
        assert_eq!(out, v(&["x", "x", "y"]));
    }

    // Checks encoded blobs become stubs and long plain words stay.
    #[test]
    fn test_replace_blobs() {
        let blob = "aB3+".repeat(60);
        let (out, changed) = replace_blobs(vec![format!("img: data:image/png;base64,{blob}== end")]);
        assert!(changed);
        assert_eq!(out, vec!["img: data:image/png;base64,<blob 242 chars> end".to_string()]);
        let digits = "7".repeat(300);
        assert_eq!(replace_blobs(vec![digits.clone()]), (vec![digits], false));
        let word = "a".repeat(300);
        let (out, changed) = replace_blobs(vec![word.clone()]);
        assert!(!changed);
        assert_eq!(out, vec![word]);
    }

    // Checks over-long lines are clipped with their original length.
    #[test]
    fn test_clip_long() {
        let (out, changed) = clip_long(vec!["x".repeat(1000), "short".into()]);
        assert!(changed);
        assert!(out[0].ends_with("… [line was 1000 bytes]") && out[0].len() < 340);
        assert_eq!(out[1], "short");
        assert!(!clip_long(vec!["ok".into()]).1);
    }

    // Checks the budget keeps head and tail, clips giant lines and leaves small input alone.
    #[test]
    fn test_fit_budget() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let (out, changed) = fit_budget(lines, 100);
        assert!(changed);
        assert_eq!(out[0], "line 0");
        assert_eq!(out.last().map(String::as_str), Some("line 199"));
        assert!(out.iter().any(|l| l.contains("cut to fit the budget of 100")));
        let (out, changed) = fit_budget(vec!["y".repeat(5000)], 100);
        assert!(changed && out[0].ends_with("[line was 5000 bytes]"));
        assert_eq!(fit_budget(v(&["a"]), 100), (v(&["a"]), false));
    }

    // Checks grep output end to end: colours removed, grouped, and the stats footer.
    #[test]
    fn test_run_grep_with_stats() {
        let input = "\x1b[35msrc/a.rs\x1b[0m:1:foo\n\x1b[35msrc/a.rs\x1b[0m:2:foo bar\nsrc/a.rs:3:foo\n";
        let o = go(&["--stats"], input);
        assert_eq!(o.status, 0);
        assert!(o.text.starts_with("src/a.rs\n1: foo\n2: foo bar\n3: foo\n[clean ~"), "{}", o.text);
        assert!(o.text.ends_with("steps: ansi,grep]"));
        let o = go(&["--as", "text", "--stats"], "src/a.rs:1:a\nsrc/a.rs:2:b\n");
        assert_eq!(o.text.lines().next(), Some("src/a.rs:1:a"));
        assert!(o.text.ends_with("steps: none]"));
    }

    // Checks JSON input is minified, not clipped, and that --as json falls back on text.
    #[test]
    fn test_run_json() {
        let big = format!("{{\n  \"k\": \"{}\",\n  \"n\": 1\n}}\n", "word ".repeat(200));
        let o = go(&[], &big);
        assert!(o.text.starts_with("{\"k\":\"word word") && o.text.ends_with(",\"n\":1}"));
        assert_eq!(go(&["--as", "json"], "plain  \n").text, "plain");
        assert_eq!(go(&["--as", "grep"], "{ \"a\": 1 }").text, "{ \"a\": 1 }");
    }

    // Checks --lossless keeps progress lines, blobs and long lines but still tidies.
    #[test]
    fn test_run_lossless() {
        let input =
            format!("10% |█████     |\n100% |██████████|\n{}\n{}  \n", "q1".repeat(150), "z".repeat(700));
        let o = go(&["--lossless", "--stats"], &input);
        assert!(o.text.contains("10% |") && o.text.contains(&"q1".repeat(150)));
        assert!(o.text.contains(&"z".repeat(700)) && o.text.ends_with("steps: whitespace]"));
        let o = go(&["--stats"], &input);
        assert!(o.text.contains("… 1 progress lines dropped") && o.text.contains("<blob 300 chars>"));
        assert!(o.text.contains("[line was 700 bytes]"));
    }

    // Checks the budget option, the empty case and the never-larger rule.
    #[test]
    fn test_run_budget_empty_and_never_larger() {
        let input: String = (0..500).map(|i| format!("row {i}\n")).collect();
        let o = go(&["--budget", "50"], &input);
        assert!(o.text.contains("cut to fit the budget of 50") && o.text.ends_with("row 499"));
        assert_eq!(go(&[], "").text, "(empty)");
        assert_eq!(go(&[], "\n\n").text, "(empty)");
        let c = clean("a\nb\r\n", Kind::Auto, true, 100);
        assert_eq!((c.lines, c.steps), (v(&["a", "b"]), vec!["whitespace"]));
        let blob = "aB3".repeat(70);
        let c = clean(&blob, Kind::Text, true, 100);
        assert_eq!(c.lines, v(&["<blob 210 chars>"]));
        let c = clean("ab", Kind::Text, true, 100);
        assert!(c.steps.is_empty());
    }

    // Checks the never-larger rule restores the input when markers would cost more than they save.
    #[test]
    fn test_clean_never_larger() {
        let c = clean("50% =====\n60% =====\n", Kind::Text, true, 1000);
        assert_eq!((c.lines, c.steps), (v(&["50% =====", "60% ====="]), Vec::<&str>::new()));
    }

    // Checks argument errors: positional paths, a bad --as, a zero budget and missing stdin.
    #[test]
    fn test_run_errors() {
        assert!(go(&["file.txt"], "").text.starts_with("error: clean reads standard input only; usage:"));
        assert_eq!(go(&["--as", "x"], "").text, "error: --as must be auto, grep, json or text, not 'x'");
        assert_eq!(go(&["--budget", "0"], "a").text, "error: --budget must be at least 1");
        assert!(go(&["--bogus"], "").text.contains("usage: CMD | zazzle clean"));
        let t = crate::testutil::TempDir::new();
        assert_eq!(crate::testutil::run_cmd(t.path(), &["clean"]).text, "error: no standard input available");
    }

    // Checks the library entry point detects JSON and joins lines.
    #[test]
    fn test_shrink() {
        assert_eq!(shrink("{ \"a\": 1 }", true, 100), "{\"a\":1}");
        assert_eq!(shrink("x  \r\ny\n", false, 100), "x\ny");
    }

    // Checks tidy_lines strips escapes and redraws but leaves grep-like lines ungrouped.
    #[test]
    fn test_tidy_lines() {
        assert_eq!(tidy_lines("\x1b[1ma.rs:1:x\x1b[0m  \na.rs:2:y\r\n"), v(&["a.rs:1:x", "a.rs:2:y"]));
        assert_eq!(tidy_lines("p 1%\rp 100%\n\n\n"), v(&["p 100%"]));
    }

    // Checks input over the size cap is cut and says so.
    #[test]
    fn test_run_input_cap() {
        let e = Engine::new();
        let big = vec![b'a'; MAX_INPUT + 10];
        let ctx = Ctx {
            engine: &e,
            cwd: PathBuf::from("/"),
            session: String::new(),
            stdin: RefCell::new(Some(Box::new(std::io::Cursor::new(big)))),
            caller: RefCell::new(None),
        };
        let o = run(&ctx, &[]).expect("clean");
        assert!(o.text.ends_with("… input cut at 64 MiB"));
    }
}
