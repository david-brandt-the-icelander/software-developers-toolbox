// The read command: numbered lines of files or ranges, bounded by a token budget.
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::indexer::Indexer;
use crate::markup;
use crate::meta::{anchor_str, line_anchor, line_bytes, Meta};
use crate::outline;
use crate::symbols::{self, Miss};
use crate::tokens;
use crate::util::{clip, display, hex8, resolve};

const SPEC: &[Flag] = &[
    opt("around", 'a'),
    opt("ctx", '\0'),
    opt("head", 'n'),
    opt("tail", 't'),
    opt("budget", 'b'),
    opt("symbol", 's'),
    flag("anchors", '\0'),
    flag("source", '\0'),
    flag("full", '\0'),
];
const USAGE: &str = "usage: read PATH[:A-B].. [--source] [--symbol NAME[#N]] [--around N] [--ctx N] [--head N] [--tail N] [--budget T] [--anchors] [--full]";
const DEFAULT_BUDGET: usize = 3000;
const DEFAULT_CTX: usize = 20;
const MINIFIED_CLIP: usize = 400;

// Which part of a file to show.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Select {
    Whole,
    Range(usize, Option<usize>),
    Around(usize),
    Head(usize),
    Tail(usize),
}

// Parses the text after a colon into a start line and optional end line.
pub(super) fn parse_range(r: &str) -> Option<(usize, Option<usize>)> {
    let (a, b) = match r.split_once('-') {
        Some((a, "")) => (a.parse().ok()?, None),
        Some((a, b)) => (a.parse().ok()?, Some(b.parse().ok()?)),
        None => {
            let a: usize = r.parse().ok()?;
            (a, Some(a))
        }
    };
    (a >= 1).then_some((a, b))
}

// Splits PATH:A-B into the path and an optional range.
fn split_target(arg: &str) -> (&str, Option<(usize, Option<usize>)>) {
    if let Some(i) = arg.rfind(':') {
        if i > 0 {
            if let Some(range) = parse_range(&arg[i + 1..]) {
                return (&arg[..i], Some(range));
            }
        }
    }
    (arg, None)
}

// Resolves the selection into an inclusive 1-based line range within the file.
fn bounds(sel: Select, nl: usize, ctx: usize) -> Result<(usize, usize), String> {
    let (a, b) = match sel {
        Select::Whole => (1, nl),
        Select::Range(a, b) => (a, b.unwrap_or(nl)),
        Select::Around(n) => (n.saturating_sub(ctx).max(1), n + ctx),
        Select::Head(n) => (1, n),
        Select::Tail(n) => (nl.saturating_sub(n) + 1, nl),
    };
    if a > nl {
        return Err(format!("start line {a} is past the end ({nl} lines)"));
    }
    if b < a {
        return Err(format!("range {a}-{b} is backwards"));
    }
    Ok((a, b.min(nl)))
}

// Chooses which lines to keep within the budget: a head part, a tail part and the elided count between.
pub(super) fn fit(costs: &[usize], budget: usize) -> (usize, usize) {
    let total: usize = costs.iter().sum();
    if total <= budget {
        return (costs.len(), costs.len());
    }
    let head_budget = budget * 7 / 10;
    let (mut head, mut used) = (0, 0);
    while used + costs[head] <= head_budget {
        used += costs[head];
        head += 1;
    }
    let head = head.max(1);
    let (mut tail_start, mut used) = (costs.len(), 0);
    while tail_start > head && used + costs[tail_start - 1] <= budget - head_budget {
        used += costs[tail_start - 1];
        tail_start -= 1;
    }
    (head, tail_start)
}

// Formats one numbered line.
fn format_line(data: &[u8], meta: &Meta, n: usize, anchors: bool) -> String {
    let text = String::from_utf8_lossy(line_bytes(data, &meta.starts, n));
    let text = if meta.minified { clip(&text, 0, MINIFIED_CLIP) } else { text.into_owned() };
    if anchors {
        format!("{n}#{}\t{text}", anchor_str(line_anchor(data, &meta.starts, n)))
    } else {
        format!("{n}\t{text}")
    }
}

// Settings shared by every file in one read call.
#[derive(Clone, Copy)]
struct Opts {
    sel: Select,
    ctx: usize,
    budget: usize,
    anchors: bool,
    terse: bool,
}

// Renders one file according to the options; the label names a symbol in the header.
fn render(ctx: &Ctx, arg: &str, o: &Opts, label: &str) -> Result<String, String> {
    let (path_arg, range) = split_target(arg);
    let sel = match (o.sel, range) {
        (Select::Whole, Some((a, b))) => Select::Range(a, b),
        (s, None) => s,
        (_, Some(_)) => return Err("give either PATH:A-B or --around/--head/--tail, not both".into()),
    };
    let path = resolve(path_arg, &ctx.cwd);
    let shown = display(&path, &ctx.cwd);
    let loaded = ctx.engine.load(&path).map_err(|e| format!("{shown}: {e}"))?;
    let (data, meta) = (&loaded.data, &loaded.meta);
    let sha = hex8(&meta.hash);
    if meta.binary {
        return Ok(format!("{shown}: binary, {} bytes, sha {sha}", meta.size));
    }
    let nl = meta.lines();
    if nl == 0 {
        return Ok(format!("{shown}: empty, sha {sha}"));
    }
    let (a, b) = bounds(sel, nl, o.ctx).map_err(|e| format!("{shown}: {e}"))?;
    let mut unavailable = String::new();
    if o.terse && outline::lang_of(&path) == outline::Lang::CSharp {
        match Indexer::locate().and_then(|ix| ix.view(&path)) {
            Ok(view) => return Ok(render_terse(&shown, &view, (a, b), nl, o, label)),
            Err(e) => unavailable = format!("(no terse view: {e}; this is the source)\n"),
        }
    }
    if let Some(view) = markup::kind_of(&path)
        .filter(|_| o.terse)
        .and_then(|k| markup::view(&shown, &String::from_utf8_lossy(data), k))
    {
        return Ok(render_terse(&shown, &view, (a, b), nl, o, label));
    }
    let costs: Vec<usize> =
        (a..=b).map(|n| tokens::from_bytes(line_bytes(data, &meta.starts, n).len() + 8)).collect();
    let (head, tail_start) = fit(&costs, o.budget);
    let partial = if (a, b) == (1, nl) { String::new() } else { format!(" [{a}-{b}{label}]") };
    let mut out = format!("{shown} {nl} lines, sha {sha}{partial}\n{unavailable}");
    for i in 0..head {
        out.push_str(&format_line(data, meta, a + i, o.anchors));
        out.push('\n');
    }
    if tail_start > head {
        let (x, y) = (a + head, a + tail_start - 1);
        out.push_str(&format!("… {} lines elided ({x}-{y}); read {shown}:{x}-{y} or --full\n", y - x + 1));
    }
    for i in tail_start.max(head)..costs.len() {
        out.push_str(&format_line(data, meta, a + i, o.anchors));
        out.push('\n');
    }
    out.pop();
    Ok(out)
}

// Returns the source lines a terse view line covers, from its key: `N`, `N~` or `A-B`.
fn view_key(key: &str) -> Option<(usize, usize)> {
    let k = key.trim_end_matches('~');
    match k.split_once('-') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => k.parse().ok().map(|n| (n, n)),
    }
}

// Renders the indexer's terse view of a C# file for a line range under the budget; it has no sha, so edits need --source.
fn render_terse(shown: &str, view: &str, (a, b): (usize, usize), nl: usize, o: &Opts, label: &str) -> String {
    let (head, body) = view.split_once('\n').unwrap_or((view, ""));
    let steps = head.split_once("terse: ").map_or("nothing to shorten", |(_, s)| s);
    let lines: Vec<(usize, usize, &str)> = body
        .lines()
        .filter_map(|l| view_key(l.split_once('\t')?.0).map(|(s, e)| (s, e, l)))
        .filter(|(s, e, _)| *e >= a && *s <= b)
        .collect();
    let costs: Vec<usize> = lines.iter().map(|(_, _, l)| tokens::from_bytes(l.len() + 1)).collect();
    let (keep, tail_start) = fit(&costs, o.budget);
    let partial = if (a, b) == (1, nl) { String::new() } else { format!(" [{a}-{b}{label}]") };
    let mut out =
        format!("{shown} {nl} lines, terse view{partial}: {steps}; read --source for the code and its sha\n");
    for (_, _, l) in &lines[..keep] {
        out.push_str(l);
        out.push('\n');
    }
    if tail_start > keep {
        let (x, y) = (lines[keep].0, lines[tail_start - 1].1);
        out.push_str(&format!("… lines {x}-{y} elided; read {shown}:{x}-{y} or --full\n"));
    }
    for (_, _, l) in &lines[tail_start.max(keep)..] {
        out.push_str(l);
        out.push('\n');
    }
    out.pop();
    out
}

// Explains a symbol name that did not resolve, listing what can be asked for instead.
fn miss_text(shown: &str, query: &str, miss: Miss) -> String {
    match miss {
        Miss::None(near) if near.is_empty() => {
            format!("{shown}: no symbol '{query}'; list them with zazzle outline {shown}")
        }
        Miss::None(near) => format!("{shown}: no symbol '{query}'; similar:\n{}", near.join("\n")),
        Miss::Many(n, list) => {
            format!(
                "{shown}: '{query}' matches {n} symbols; pick one with its parameter types, Name(T1, T2), or #N:\n{}",
                list.join("\n")
            )
        }
        Miss::Index(n) => format!("{shown}: '{query}' has {n} match(es); pick #1 to #{n}"),
    }
}

// Renders one named symbol with the doc comments and attributes directly above it.
fn render_symbol(ctx: &Ctx, arg: &str, query: &str, o: &Opts) -> Result<String, String> {
    if split_target(arg).1.is_some() {
        return Err("give either PATH:A-B or --symbol, not both".into());
    }
    let path = resolve(arg, &ctx.cwd);
    let shown = display(&path, &ctx.cwd);
    let loaded = ctx.engine.load(&path).map_err(|e| format!("{shown}: {e}"))?;
    if loaded.meta.binary {
        return render(ctx, arg, o, "");
    }
    let syms = ctx.engine.outline(&path, &loaded);
    let (data, starts) = (&loaded.data, &loaded.meta.starts);
    let line = |n: usize| String::from_utf8_lossy(line_bytes(data, starts, n)).into_owned();
    let s = &syms[symbols::find(&syms, query, &line).map_err(|m| miss_text(&shown, query, m))?];
    let above: Vec<&str> =
        (1..s.start).map(|n| std::str::from_utf8(line_bytes(data, starts, n)).unwrap_or("")).collect();
    let first = symbols::lead_start(&above, s.start, outline::lang_of(&path));
    let picked = Opts { sel: Select::Range(first, Some(s.end)), ..*o };
    render(ctx, arg, &picked, &format!(" {} {}", s.kind, s.name))
}

// Builds the selection from the flags, rejecting conflicts.
fn selection(p: &args::Parsed) -> Result<Select, String> {
    let picks = [("around", p.number("around")?), ("head", p.number("head")?), ("tail", p.number("tail")?)];
    let given: Vec<_> = picks.iter().filter(|(_, v)| v.is_some()).collect();
    if given.len() > 1 {
        return Err("use only one of --around, --head, --tail".into());
    }
    Ok(match given.first() {
        Some(("around", Some(n))) => Select::Around(*n),
        Some(("head", Some(n))) => Select::Head(*n),
        Some((_, Some(n))) => Select::Tail(*n),
        _ => Select::Whole,
    })
}

// Runs the read command.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    if p.positional.is_empty() {
        return Err(USAGE.into());
    }
    let budget = match p.number("budget")? {
        Some(0) => return Err("--budget must be at least 1".into()),
        Some(n) => n,
        None if p.has("full") => usize::MAX,
        None => DEFAULT_BUDGET,
    };
    let opts = Opts {
        sel: selection(&p)?,
        ctx: p.number("ctx")?.unwrap_or(DEFAULT_CTX),
        budget,
        anchors: p.has("anchors"),
        terse: !p.has("source") && !p.has("anchors"),
    };
    let symbol = p.value("symbol");
    if symbol.is_some() && opts.sel != Select::Whole {
        return Err("--symbol picks the lines itself; drop --around, --head and --tail".into());
    }
    let mut parts = Vec::new();
    for arg in &p.positional {
        parts.push(match symbol {
            Some(q) => render_symbol(ctx, arg, q, &opts)?,
            None => render(ctx, arg, &opts, "")?,
        });
    }
    Ok(Output::ok(parts.join("\n\n")))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{run_cmd, TempDir};

    // Writes a numbered 10-line file.
    fn ten() -> TempDir {
        let t = TempDir::new();
        let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        t.write("f.txt", body.as_bytes());
        t
    }

    // Checks reading a whole file shows a header and numbered lines.
    #[test]
    fn test_read_whole_file() {
        let t = ten();
        let o = run_cmd(t.path(), &["read", "f.txt"]);
        let lines: Vec<&str> = o.text.lines().collect();
        assert!(lines[0].starts_with("f.txt 10 lines, sha ") && !lines[0].contains('['));
        assert_eq!(lines[1], "1\tline1");
        assert_eq!(lines.len(), 11);
        assert!(o.idable);
    }

    // Checks ranges: closed, open-ended and single-line.
    #[test]
    fn test_read_ranges() {
        let t = ten();
        let a = run_cmd(t.path(), &["read", "f.txt:3-4"]).text;
        assert!(a.contains("[3-4]") && a.ends_with("3\tline3\n4\tline4"));
        let b = run_cmd(t.path(), &["read", "f.txt:9-"]).text;
        assert!(b.ends_with("9\tline9\n10\tline10"));
        let c = run_cmd(t.path(), &["read", "f.txt:5"]).text;
        assert!(c.ends_with("5\tline5") && c.contains("[5-5]"));
    }

    // Checks around, head and tail selections.
    #[test]
    fn test_read_around_head_tail() {
        let t = ten();
        let a = run_cmd(t.path(), &["read", "f.txt", "--around", "5", "--ctx", "1"]).text;
        assert!(a.ends_with("4\tline4\n5\tline5\n6\tline6") && a.contains("[4-6]"));
        let h = run_cmd(t.path(), &["read", "f.txt", "-n", "2"]).text;
        assert!(h.ends_with("1\tline1\n2\tline2"));
        let tl = run_cmd(t.path(), &["read", "f.txt", "-t", "2"]).text;
        assert!(tl.ends_with("9\tline9\n10\tline10"));
        let near_top = run_cmd(t.path(), &["read", "f.txt", "-a", "1", "--ctx", "2"]).text;
        assert!(near_top.contains("[1-3]"));
    }

    // Checks anchors appear with --anchors.
    #[test]
    fn test_read_anchors() {
        let t = ten();
        let o = run_cmd(t.path(), &["read", "f.txt:1-1", "--anchors"]).text;
        let line = o.lines().nth(1).expect("line");
        assert!(line.starts_with("1#") && line.ends_with("\tline1") && line.len() == "1#abc\tline1".len());
    }

    // Checks the budget elides the middle with a hint.
    #[test]
    fn test_read_budget_elides_middle() {
        let t = TempDir::new();
        let body: String = (1..=200).map(|i| format!("this is line number {i}\n")).collect();
        t.write("big.txt", body.as_bytes());
        let o = run_cmd(t.path(), &["read", "big.txt", "--budget", "100"]).text;
        assert!(o.contains("lines elided (") && o.contains("or --full"));
        assert!(o.contains("1\tthis is line number 1") && o.contains("200\tthis is line number 200"));
        let full = run_cmd(t.path(), &["read", "big.txt", "--full"]).text;
        assert!(!full.contains("elided") && full.lines().count() == 201);
    }

    // Checks the default budget applies to large files.
    #[test]
    fn test_read_default_budget() {
        let t = TempDir::new();
        let body: String = (1..=2000).map(|i| format!("this is line number {i}\n")).collect();
        t.write("big.txt", body.as_bytes());
        assert!(run_cmd(t.path(), &["read", "big.txt"]).text.contains("lines elided"));
    }

    // Checks binary, empty and minified files.
    #[test]
    fn test_read_special_files() {
        let t = TempDir::new();
        t.write("b.bin", b"a\0b");
        t.write("e.txt", b"");
        t.write("m.js", format!("{}\n", "x".repeat(6000)).as_bytes());
        assert!(run_cmd(t.path(), &["read", "b.bin"]).text.starts_with("b.bin: binary, 3 bytes, sha "));
        assert!(run_cmd(t.path(), &["read", "e.txt"]).text.starts_with("e.txt: empty, sha "));
        let m = run_cmd(t.path(), &["read", "m.js"]).text;
        assert!(m.contains('…') && m.len() < 700);
    }

    // Checks multiple files are joined with a blank line.
    #[test]
    fn test_read_multiple_files() {
        let t = ten();
        t.write("g.txt", b"only\n");
        let o = run_cmd(t.path(), &["read", "g.txt", "f.txt:1-1"]).text;
        assert!(o.contains("1\tonly\n\nf.txt 10 lines"));
    }

    // Checks every error message.
    #[test]
    fn test_read_errors() {
        let t = ten();
        let e = |args: &[&str]| run_cmd(t.path(), args).text;
        assert!(e(&["read"]).starts_with("error: usage: read"));
        assert!(e(&["read", "f.txt", "--nope"]).contains("unknown flag"));
        assert_eq!(e(&["read", "f.txt:20-30"]), "error: f.txt: start line 20 is past the end (10 lines)");
        assert_eq!(e(&["read", "f.txt:5-2"]), "error: f.txt: range 5-2 is backwards");
        assert_eq!(e(&["read", "nope.txt"]), "error: nope.txt: no such file or directory");
        assert_eq!(e(&["read", "f.txt", "--budget", "0"]), "error: --budget must be at least 1");
        assert_eq!(
            e(&["read", "f.txt", "-n", "1", "-t", "1"]),
            "error: use only one of --around, --head, --tail"
        );
        assert!(e(&["read", "f.txt:2-3", "-n", "1"]).contains("not both"));
        assert!(e(&["read", "f.txt", "--budget", "x"]).contains("needs a number"));
    }

    // Checks target splitting and range parsing edge cases.
    #[test]
    fn test_split_target_and_ranges() {
        assert_eq!(split_target("a.rs:3-5"), ("a.rs", Some((3, Some(5)))));
        assert_eq!(split_target("a.rs:3-"), ("a.rs", Some((3, None))));
        assert_eq!(split_target("a.rs:7"), ("a.rs", Some((7, Some(7)))));
        assert_eq!(split_target("a.rs:0"), ("a.rs:0", None));
        assert_eq!(split_target("a:b"), ("a:b", None));
        assert_eq!(split_target(":5"), (":5", None));
        assert_eq!(split_target("plain"), ("plain", None));
        assert_eq!(parse_range("x-3"), None);
        assert_eq!(parse_range("3-x"), None);
    }

    // Checks the budget fitting maths directly.
    #[test]
    fn test_fit_cases() {
        assert_eq!(fit(&[1, 1, 1], 10), (3, 3));
        let costs = vec![10; 20];
        let (head, tail) = fit(&costs, 100);
        assert_eq!(head, 7);
        assert_eq!(tail, 17);
        assert_eq!(fit(&[50, 50], 10), (1, 2));
    }

    // Checks fitting when one huge head line leaves the tail reaching the head.
    #[test]
    fn test_fit_tail_reaches_head() {
        assert_eq!(fit(&[100, 1, 1], 50), (1, 1));
    }

    // Writes a Rust file with a documented, attributed method in each of two impls.
    fn rust_file() -> TempDir {
        let t = TempDir::new();
        let src = "struct A;\n\nimpl A {\n    /// Makes one.\n    #[inline]\n    fn new() -> A {\n        A\n    }\n}\n\nstruct B;\n\nimpl B {\n    fn new() -> B {\n        B\n    }\n}\n";
        t.write("m.rs", src.as_bytes());
        t
    }

    // Checks a qualified name reads the symbol with its doc comment and attribute, labelled in the header.
    #[test]
    fn test_symbol_reads_with_docs() {
        let t = rust_file();
        let o = run_cmd(t.path(), &["read", "m.rs", "--symbol", "A.new"]);
        assert_eq!(
            o.text,
            "m.rs 17 lines, sha {SHA} [4-8 fn new]\n4\t    /// Makes one.\n5\t    #[inline]\n6\t    fn new() -> A {\n7\t        A\n8\t    }"
                .replace("{SHA}", &o.text[o.text.find("sha ").expect("sha") + 4..][..8])
        );
        let picked = run_cmd(t.path(), &["read", "m.rs", "-s", "new#2"]).text;
        assert!(picked.contains("[14-16 fn new]") && picked.contains("14\t    fn new() -> B {"));
        let both = run_cmd(t.path(), &["read", "m.rs", "m.rs", "-s", "B::new"]).text;
        assert_eq!(both.matches("[14-16 fn new]").count(), 2);
    }

    // Checks the explanations for ambiguous, unknown and out-of-range names.
    #[test]
    fn test_symbol_misses() {
        let t = rust_file();
        let many = run_cmd(t.path(), &["read", "m.rs", "-s", "new"]);
        assert_eq!(many.status, 2);
        assert_eq!(
            many.text,
            "error: m.rs: 'new' matches 2 symbols; pick one with its parameter types, Name(T1, T2), or #N:\n#1 6-8 fn A.new: fn new() -> A {\n#2 14-16 fn B.new: fn new() -> B {"
        );
        assert_eq!(
            run_cmd(t.path(), &["read", "m.rs", "-s", "ne"]).text,
            "error: m.rs: no symbol 'ne'; similar:\n6-8 fn A.new: fn new() -> A {\n14-16 fn B.new: fn new() -> B {"
        );
        assert_eq!(
            run_cmd(t.path(), &["read", "m.rs", "-s", "zzz"]).text,
            "error: m.rs: no symbol 'zzz'; list them with zazzle outline m.rs"
        );
        assert_eq!(
            run_cmd(t.path(), &["read", "m.rs", "-s", "new#3"]).text,
            "error: m.rs: 'new#3' has 2 match(es); pick #1 to #2"
        );
    }

    // Checks --symbol refuses other selections, reports load errors and leaves binary files to the plain reader.
    #[test]
    fn test_symbol_conflicts_and_files() {
        let t = rust_file();
        assert_eq!(
            run_cmd(t.path(), &["read", "m.rs", "-s", "A", "--head", "2"]).text,
            "error: --symbol picks the lines itself; drop --around, --head and --tail"
        );
        assert_eq!(
            run_cmd(t.path(), &["read", "m.rs:1-2", "-s", "A"]).text,
            "error: give either PATH:A-B or --symbol, not both"
        );
        assert!(run_cmd(t.path(), &["read", "nope.rs", "-s", "A"]).text.starts_with("error: nope.rs: "));
        t.write("b.bin", b"\0\x01\x02");
        assert!(run_cmd(t.path(), &["read", "b.bin", "-s", "A"]).text.starts_with("b.bin: binary, 3 bytes"));
    }

    // Points the indexer bridge at a fake daemon while a test runs, and writes a C# file in a git work tree.
    fn csharp(
        fake: &crate::indexer::tests::FakeIndexer,
    ) -> (TempDir, Vec<(&'static str, Option<std::ffi::OsString>)>) {
        let saved = ["ZAZZLE_INDEXER", "ZAZZLE_INDEXER_STATE_DIR"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        std::env::set_var("ZAZZLE_INDEXER", &fake.exe);
        std::env::set_var("ZAZZLE_INDEXER_STATE_DIR", fake.dir.path());
        let t = TempDir::new();
        std::fs::create_dir_all(t.path().join(".git")).expect("git dir");
        t.write("C.cs", b"using A;\nusing B;\n\npublic class C\n{\n}\n");
        (t, saved)
    }

    // Puts back the environment a test changed.
    fn restore(saved: Vec<(&'static str, Option<std::ffi::OsString>)>) {
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    // Checks HTML and Razor files read as the markup terse view, files with nothing to shorten as source, and --source gives the code.
    #[test]
    fn test_markup_terse_view() {
        let t = TempDir::new();
        std::fs::write(t.path().join("a.html"), "<html>\n<!--\nnote\n-->\n<p>hi</p>\n").expect("write");
        std::fs::write(t.path().join("b.cshtml"), "@using A\n@using B\n<p class=\"a b c d e\">x</p>\n")
            .expect("write");
        std::fs::write(t.path().join("c.htm"), "<p>plain</p>\n").expect("write");
        assert_eq!(
            run_cmd(t.path(), &["read", "a.html"]).text,
            "a.html 5 lines, terse view: 1 comments; read --source for the code and its sha\n1\t<html>\n2-4\t⋯ comment, 3 line(s)\n5\t<p>hi</p>"
        );
        let razor = run_cmd(t.path(), &["read", "b.cshtml:3-3"]).text;
        assert!(
            razor.starts_with("b.cshtml 3 lines, terse view [3-3]: 1 usings, 1 long attributes;")
                && razor.ends_with("\n3~\t<p class=\"a b c …\">x</p>"),
            "{razor}"
        );
        assert!(run_cmd(t.path(), &["read", "c.htm"]).text.starts_with("c.htm 1 lines, sha "));
        assert!(run_cmd(t.path(), &["read", "a.html", "--source"]).text.contains("3\tnote"));
    }

    // Checks a C# file reads as the terse view with no sha, by range and by symbol, and --source gives the code and sha.
    #[test]
    fn test_csharp_terse_view() {
        let _g = crate::testutil::env_lock();
        let fake = crate::indexer::tests::FakeIndexer::start(crate::indexer::tests::viewing);
        let (t, saved) = csharp(&fake);
        let whole = run_cmd(t.path(), &["read", "C.cs"]).text;
        assert_eq!(
            whole,
            "C.cs 6 lines, terse view: 1 usings, 1 access modifiers; read --source for the code and its sha\n1-2\t⋯ 2 using(s)\n3\t\n4~\tclass C\n5\t{\n6\t}"
        );
        let part = run_cmd(t.path(), &["read", "C.cs:4-5"]).text;
        assert!(
            part.starts_with("C.cs 6 lines, terse view [4-5]: ") && part.ends_with("\n4~\tclass C\n5\t{"),
            "{part}"
        );
        let sym = run_cmd(t.path(), &["read", "C.cs", "--symbol", "C"]).text;
        assert!(sym.contains("[4-6 class C]") && sym.ends_with("4~\tclass C\n5\t{\n6\t}"), "{sym}");
        let source = run_cmd(t.path(), &["read", "C.cs", "--source"]).text;
        assert!(source.starts_with("C.cs 6 lines, sha ") && source.contains("4\tpublic class C"));
        assert!(run_cmd(t.path(), &["read", "C.cs", "--anchors"]).text.starts_with("C.cs 6 lines, sha "));
        let small = run_cmd(t.path(), &["read", "C.cs", "--budget", "8"]).text;
        assert!(small.contains("… lines ") && small.contains(" elided; read C.cs:"), "{small}");
        restore(saved);
    }

    // Checks the source comes back, saying why, when the indexer cannot give a view.
    #[test]
    fn test_csharp_view_unavailable() {
        let _g = crate::testutil::env_lock();
        let fake = crate::indexer::tests::FakeIndexer::start(crate::indexer::tests::viewing);
        let (t, saved) = csharp(&fake);
        std::env::set_var("ZAZZLE_INDEXER", t.path().join("missing-indexer"));
        let out = run_cmd(t.path(), &["read", "C.cs"]).text;
        assert!(out.starts_with("C.cs 6 lines, sha "), "{out}");
        assert!(out.contains("\n(no terse view: zazzle-csharp-indexer --version: "), "{out}");
        assert!(out.contains("4\tpublic class C"));
        restore(saved);
    }

    // Checks view line keys parse into the source lines they cover.
    #[test]
    fn test_view_key() {
        assert_eq!(view_key("12"), Some((12, 12)));
        assert_eq!(view_key("12~"), Some((12, 12)));
        assert_eq!(view_key("3-9"), Some((3, 9)));
        assert_eq!(view_key("x"), None);
        assert_eq!(view_key("3-x"), None);
        assert_eq!(
            render_terse(
                "f.cs",
                "f.cs 1 lines",
                (1, 1),
                1,
                &Opts { sel: Select::Whole, ctx: 0, budget: 100, anchors: false, terse: true },
                ""
            ),
            "f.cs 1 lines, terse view: nothing to shorten; read --source for the code and its sha"
        );
    }
}
