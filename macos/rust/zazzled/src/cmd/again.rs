// The read-again command: a response sent in this session, rerun from its request, or a stored output of clean, run or proxy, as shown or in full.
use super::read::{fit, parse_range};
use super::{clean, produce, Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::outputs;
use crate::tokens;
use std::cell::RefCell;

const SPEC: &[Flag] = &[flag("full", '\0'), opt("lines", '\0'), opt("budget", 'b')];
const USAGE: &str = "usage: read-again r:ID [--full [--lines A-B] [--budget T]]";
const DEFAULT_BUDGET: usize = 3000;

// Reruns the request behind a session response: the same text under the same id if nothing changed, else the current version under a new id; None when the session does not know the id.
fn rerun(ctx: &Ctx, id: &str) -> Option<Output> {
    if ctx.session.is_empty() {
        return None;
    }
    let (replay, desc) = ctx.engine.sessions.replay(&ctx.session, id)?;
    let (cmd, args) = replay.args.split_first()?;
    let again = Ctx {
        engine: ctx.engine,
        cwd: replay.cwd.clone(),
        session: ctx.session.clone(),
        stdin: RefCell::new(None),
        caller: RefCell::new(None),
    };
    let (mut out, _, _) = produce(&again, cmd, args);
    if out.idable {
        out.text = ctx.engine.sessions.resend(&ctx.session, id, &desc, replay.clone(), out.text);
    }
    Some(out)
}

// Shows a stored output's full text as numbered lines for a range, keeping head and tail within the budget.
fn render_full(id: &str, full: &str, lines: Option<&str>, budget: usize) -> Result<Output, String> {
    let all = clean::plain_lines(full);
    let n = all.len();
    if n == 0 {
        return Ok(Output::plain(format!("r:{id} full output: empty")));
    }
    let (a, b) = match lines {
        None => (1, n),
        Some(r) => {
            let (a, b) = parse_range(r).ok_or_else(|| format!("bad line range '{r}'; use A-B"))?;
            if a > n {
                return Err(format!("r:{id} has {n} lines; line {a} is past the end"));
            }
            (a, b.unwrap_or(n).min(n).max(a))
        }
    };
    let rows: Vec<String> = (a..=b).map(|i| format!("{i}\t{}", all[i - 1])).collect();
    let costs: Vec<usize> = rows.iter().map(|r| tokens::from_bytes(r.len() + 1)).collect();
    let (head, tail) = fit(&costs, budget);
    let part = if (a, b) == (1, n) { String::new() } else { format!(" [{a}-{b}]") };
    let mut out = format!("r:{id} full output, {n} lines{part}\n");
    out.push_str(&rows[..head].join("\n"));
    if tail > head {
        let (x, y) = (a + head, a + tail - 1);
        out.push_str(&format!("\n… lines {x}-{y} elided; read-again r:{id} --full --lines {x}-{y}"));
    }
    if tail.max(head) < rows.len() {
        out.push('\n');
        out.push_str(&rows[tail.max(head)..].join("\n"));
    }
    Ok(Output::plain(out))
}

// Runs read-again: a session response rerun, or a stored output as shown or in full.
pub fn run(ctx: &Ctx, rest: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, rest).map_err(|e| format!("{e}; {USAGE}"))?;
    let [raw] = p.positional.as_slice() else { return Err(USAGE.into()) };
    let id = raw.trim_matches(['[', ']']).trim_start_matches("r:");
    let full = p.has("full") || p.has("lines");
    if !full {
        if let Some(out) = rerun(ctx, id) {
            return Ok(out);
        }
    }
    let budget = p.number("budget")?.unwrap_or(DEFAULT_BUDGET).max(1);
    match ctx.engine.outputs.as_deref().and_then(|d| outputs::load(d, id).ok()) {
        Some(s) if full => render_full(id, &s.full, p.value("lines"), budget),
        Some(s) => Ok(Output::plain(format!("[r:{id}]\n{}", s.shown))),
        None if full && ctx.engine.sessions.replay(&ctx.session, id).is_some() => {
            Err(format!("r:{id} was not cut; read-again r:{id} gives it whole"))
        }
        None => Err(format!(
            "no response r:{id} in this session or the output store (stored outputs are kept for an hour); run the command again"
        )),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::testutil::{run_cmd, run_cmd_with, TempDir};

    // Checks a session response comes back the same when unchanged and as the current version after a change.
    #[test]
    fn test_rerun_session_response() {
        let t = TempDir::new();
        t.write("a.txt", b"one\n");
        let e = Engine::new();
        let go = |args: &[&str]| run_cmd_with(&e, t.path(), "sess", args).text;
        let first = go(&["read", "a.txt", "--disable-session-deduping"]);
        let id = first[3..8].to_string();
        assert!(go(&["read", "a.txt"]).contains("already sent"));
        assert_eq!(go(&["read-again", &format!("r:{id}")]), first);
        assert_eq!(go(&["read-again", &format!("[r:{id}]")]), first);
        t.write("a.txt", b"one\ntwo\n");
        let changed = go(&["read-again", &id]);
        assert!(
            changed.contains(&format!(
                "] r:{id} changed since it was sent; this is the current version\na.txt 2 lines"
            )),
            "{changed}"
        );
        let found = go(&["find", "one"]);
        assert_eq!(go(&["read-again", &found[3..8]]), found);
        assert_eq!(
            go(&["read-again", &id, "--full"]),
            format!("error: r:{id} was not cut; read-again r:{id} gives it whole")
        );
    }

    // Checks misses and usage errors.
    #[test]
    fn test_misses() {
        let t = TempDir::new();
        let e = Engine::new();
        let go = |args: &[&str]| run_cmd_with(&e, t.path(), "sess", args).text;
        let miss = "error: no response r:zzzzz in this session or the output store (stored outputs are kept for an hour); run the command again";
        assert_eq!(go(&["read-again", "zzzzz"]), miss);
        assert_eq!(run_cmd(t.path(), &["read-again", "zzzzz"]).text, miss);
        assert_eq!(go(&["read-again"]), format!("error: {USAGE}"));
        assert!(go(&["read-again", "x", "--bogus"]).contains(USAGE));
    }

    // Runs clean over the input with an engine that stores outputs in the directory.
    fn clean_stored(e: &Engine, cwd: &std::path::Path, input: &'static [u8], args: &[&str]) -> String {
        let ctx = Ctx {
            engine: e,
            cwd: cwd.to_path_buf(),
            session: String::new(),
            stdin: RefCell::new(Some(Box::new(input))),
            caller: RefCell::new(None),
        };
        let mut all = vec!["clean".to_string()];
        all.extend(args.iter().map(|a| a.to_string()));
        super::super::run(&ctx, &all).text
    }

    // Checks a clean output is stored under an id, comes back as shown, and in full with line numbers, ranges and a budget.
    #[test]
    fn test_stored_clean_output() {
        let t = TempDir::new();
        let e = Engine::new().with_outputs(Some(t.path().join("out")));
        let input: &'static [u8] =
            b"\x1b[1mrow 1\x1b[0m\nrow 2\nrow 3\nrow 4\nrow 5\nrow 6\nrow 7\nrow 8\nrow 9\nrow 10\n";
        let shown = clean_stored(&e, t.path(), input, &["--budget", "6"]);
        assert!(shown.starts_with("[r:") && shown.ends_with(" --full]"), "{shown}");
        let id = shown[3..13].to_string();
        let go = |args: &[&str]| run_cmd_with(&e, t.path(), "", args).text;
        assert_eq!(go(&["read-again", &id]), shown);
        let full = go(&["read-again", &id, "--full"]);
        assert!(full.starts_with(&format!("r:{id} full output, 10 lines\n1\trow 1\n2\trow 2")), "{full}");
        assert_eq!(
            go(&["read-again", &id, "--lines", "9-12"]),
            format!("r:{id} full output, 10 lines [9-10]\n9\trow 9\n10\trow 10")
        );
        let small = go(&["read-again", &id, "--full", "--budget", "6"]);
        assert!(small.contains(&format!(" elided; read-again r:{id} --full --lines ")), "{small}");
        assert_eq!(
            go(&["read-again", &id, "--lines", "20"]),
            format!("error: r:{id} has 10 lines; line 20 is past the end")
        );
        assert_eq!(go(&["read-again", &id, "--lines", "x"]), "error: bad line range 'x'; use A-B");
        let whole = clean_stored(&e, t.path(), b"short\n", &[]);
        assert!(whole.starts_with("[r:") && !whole.contains("--full"), "{whole}");
        let empty = clean_stored(&e, t.path(), b"", &[]);
        assert_eq!(
            go(&["read-again", &empty[3..13], "--full"]),
            format!("r:{} full output: empty", &empty[3..13])
        );
    }

    // Checks a store that cannot be written is reported in the output instead of failing clean.
    #[test]
    fn test_unwritable_store() {
        let t = TempDir::new();
        t.write("file", b"x");
        let e = Engine::new().with_outputs(Some(t.path().join("file/out")));
        let out = clean_stored(&e, t.path(), b"hello\n", &[]);
        assert!(out.starts_with("hello\n[output not stored for read-again: "), "{out}");
    }
}
