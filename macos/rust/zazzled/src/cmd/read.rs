// The read command: numbered lines of files or ranges, bounded by a token budget.
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::meta::{anchor_str, line_anchor, line_bytes, Meta};
use crate::tokens;
use crate::util::{clip, display, hex8, resolve};

const SPEC: &[Flag] = &[
    opt("around", 'a'),
    opt("ctx", '\0'),
    opt("head", 'n'),
    opt("tail", 't'),
    opt("budget", 'b'),
    flag("anchors", '\0'),
    flag("full", '\0'),
];
const USAGE: &str =
    "usage: read PATH[:A-B].. [--around N] [--ctx N] [--head N] [--tail N] [--budget T] [--anchors] [--full]";
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
fn parse_range(r: &str) -> Option<(usize, Option<usize>)> {
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
fn fit(costs: &[usize], budget: usize) -> (usize, usize) {
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
struct Opts {
    sel: Select,
    ctx: usize,
    budget: usize,
    anchors: bool,
}

// Renders one file according to the options.
fn render(ctx: &Ctx, arg: &str, o: &Opts) -> Result<String, String> {
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
    let costs: Vec<usize> =
        (a..=b).map(|n| tokens::from_bytes(line_bytes(data, &meta.starts, n).len() + 8)).collect();
    let (head, tail_start) = fit(&costs, o.budget);
    let partial = if (a, b) == (1, nl) { String::new() } else { format!(" [{a}-{b}]") };
    let mut out = format!("{shown} {nl} lines, sha {sha}{partial}\n");
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
    };
    let mut parts = Vec::new();
    for arg in &p.positional {
        parts.push(render(ctx, arg, &opts)?);
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
}
