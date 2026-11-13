// Command dispatch: picks the subcommand, runs it and shapes failures for the wire.
use crate::engine::Engine;
use crate::session::Replay;
use crate::tokens;
use std::cell::RefCell;
use std::io::Read;
use std::path::PathBuf;

pub mod again;
pub mod clean;
pub mod edit;
pub mod find;
pub mod ls;
pub mod outline;
pub mod read;
pub mod refs;
pub mod structure;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_BUDGET: usize = 8000;
const MAX_DESC: usize = 100;

const HELP: &str = "zazzle: file tool for agents
find PATTERN [PATH..] [-i -F -w -c -l -u] [-C N] [-g GLOB] [-m N]  regex search grouped by file
read PATH[:A-B].. [--source] [--symbol NAME[#N]] [--around N] [--ctx N] [--head N] [--tail N] [--budget T] [--anchors] [--full]
ls [DIR] [-d N] [-m N] [-u]  tree with line counts
outline PATH.. [-d N] [-m N]  symbols with line ranges
refs SYMBOL [PATH..] [-g GLOB] [-m N]  definitions first, then uses with their enclosing symbol; in C# files, not in comments or strings
edit PATH --old T --new T [--all] --expect SHA | --lines A-B --new T (--expect SHA | --at A#xyz-B#xyz) | --after N --new T --expect SHA | --create --new T
CMD | clean [--as auto|grep|json|text] [--lossless] [--budget T] [--stats]  shrink piped output: colours, redraws, blank runs, repeats, grep grouping, JSON, progress bars, blobs, long lines
proxy [--lossless] [--budget T] [--skip TOOL].. [--config FILE] [--log] (-- CMD ARGS.. | --upstream URL [--listen ADDR])  MCP proxy that cleans another server's results (own process)
run [--lossless] [--budget T] [--timeout SECS] [--profile NAME|none] [--] CMD ARGS..  run a command; output cleaned with a profile for cargo, pytest, go, dotnet, jest/vitest, tsc, installs; exits with its code (own process)
read-again r:ID [--full [--lines A-B]]  a response again (same text, or the current version under a new id); clean/run/proxy outputs are stored for an hour, --full gives the uncut text
From zazzle-csharp-indexer's index of the git work tree that holds the working directory:
overview  languages, top directories, entry points, largest files
find-symbol NAME [--exact] [--kind K] [--limit N] [--offset N]  declarations by name
changed-since [CURSOR]  files added (A), modified (M) or deleted (D) since a cursor; without one, a starting cursor
git-log [PATH] [-n N] [--grep TEXT]  recent commits
git-blame PATH [--start N] [--end N]  who last changed each line
report-gap TEXT  tell the maintainers what you needed and could not get
status  daemon info
C#, Python, JavaScript, TypeScript, Dart, Rust, Go, Java, Kotlin, Swift, C, C++, HTML, Razor, CSS and SCSS files read as a terse view without a sha; --source gives the code and the sha every edit needs.
Any command: --budget T caps output at ~T tokens (default 8000; read and clean have their own); --disable-session-deduping resends output already sent in this session.";

// Where a command runs and who asked.
pub struct Ctx<'a> {
    pub engine: &'a Engine,
    pub cwd: PathBuf,
    pub session: String,
    pub stdin: RefCell<Option<Box<dyn Read>>>,
}

impl Ctx<'_> {
    // Reads all of standard input, which can be done once.
    pub fn read_stdin(&self) -> Result<Vec<u8>, String> {
        self.read_stdin_max(usize::MAX).map(|(buf, _)| buf)
    }

    // Reads standard input up to a byte limit, once; also says whether more was left unread.
    pub fn read_stdin_max(&self, max: usize) -> Result<(Vec<u8>, bool), String> {
        let Some(r) = self.stdin.borrow_mut().take() else {
            return Err("no standard input available".into());
        };
        let mut buf = Vec::new();
        r.take((max as u64).saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        let cut = buf.len() > max;
        buf.truncate(max);
        Ok((buf, cut))
    }
}

// A command result: text, exit status and whether the text may carry a response id.
#[derive(Debug, PartialEq)]
pub struct Output {
    pub text: String,
    pub status: u8,
    pub idable: bool,
}

impl Output {
    // A successful result whose text may be deduplicated within a session.
    pub fn ok(text: String) -> Output {
        Output { text, status: 0, idable: true }
    }

    // A successful result that must always be sent in full.
    pub fn plain(text: String) -> Output {
        Output { text, status: 0, idable: false }
    }

    // A failure with a one-line message.
    pub fn err(msg: &str) -> Output {
        Output { text: format!("error: {msg}"), status: 2, idable: false }
    }
}

// Options every command accepts, removed from the command line before dispatch.
struct Globals {
    dedupe: bool,
    budget: Option<usize>,
}

// Pulls the global options out of a command's arguments; read keeps its own --budget.
fn split_globals(cmd: &str, args: &[String]) -> Result<(Vec<String>, Globals), String> {
    let takes_budget = !matches!(cmd, "read" | "cat" | "clean");
    let (mut rest, mut g) = (Vec::new(), Globals { dedupe: true, budget: None });
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            rest.push(a.clone());
            rest.extend(it.by_ref().cloned());
        } else if a == "--disable-session-deduping" {
            g.dedupe = false;
        } else if takes_budget && (a == "--budget" || a.starts_with("--budget=")) {
            let value = a.strip_prefix("--budget=").map(str::to_string).or_else(|| it.next().cloned());
            let n: usize = value.and_then(|v| v.parse().ok()).ok_or("--budget needs a number")?;
            if n == 0 {
                return Err("--budget must be at least 1".into());
            }
            g.budget = Some(n);
        } else {
            rest.push(a.clone());
        }
    }
    Ok((rest, g))
}

// Cuts text at a line boundary to fit a token budget, saying how to get the rest.
fn cut_to_budget(text: String, budget: usize) -> String {
    let total = tokens::estimate(&text);
    if total <= budget {
        return text;
    }
    let (mut used, mut end) = (0, 0);
    for line in text.split_inclusive('\n') {
        let cost = tokens::from_bytes(line.len());
        if used + cost > budget && end > 0 {
            break;
        }
        used += cost;
        end += line.len();
    }
    format!(
        "{}\n… cut at ~{budget} tokens (full output ~{total}); narrow the query, or raise --budget",
        text[..end].trim_end_matches('\n')
    )
}

// Describes a command line for "already sent" notices.
fn describe(cmd: &str, rest: &[String]) -> String {
    format!("zazzle {cmd} {}", rest.join(" ")).trim_end().chars().take(MAX_DESC).collect()
}

// Runs a command and applies the output budget, without the session layer; also returns the description and dedupe choice.
pub(super) fn produce(ctx: &Ctx, cmd: &str, rest: &[String]) -> (Output, String, bool) {
    let (rest, globals) = match split_globals(cmd, rest) {
        Ok(x) => x,
        Err(e) => return (Output::err(&e), String::new(), true),
    };
    let mut out = match dispatch(ctx, cmd, &rest) {
        Ok(o) => o,
        Err(msg) => Output::err(&msg),
    };
    if out.idable && !matches!(cmd, "read" | "cat" | "clean") {
        out.text = cut_to_budget(out.text, globals.budget.unwrap_or(DEFAULT_BUDGET));
    }
    (out, describe(cmd, &rest), globals.dedupe)
}

// Runs one command line and returns its output, tagged or deduplicated for the caller's session, remembering how to run it again.
pub fn run(ctx: &Ctx, args: &[String]) -> Output {
    let Some((cmd, rest)) = args.split_first() else { return Output::plain(HELP.into()) };
    if cmd == "read-again" {
        return again::run(ctx, rest).unwrap_or_else(|e| Output::err(&e));
    }
    let (mut out, desc, dedupe) = produce(ctx, cmd, rest);
    if out.idable {
        let kept: Vec<String> = args.iter().filter(|a| *a != "--disable-session-deduping").cloned().collect();
        let replay = Replay { args: kept, cwd: ctx.cwd.clone() };
        out.text = ctx.engine.sessions.apply_with(&ctx.session, &desc, Some(replay), out.text, dedupe);
    }
    out
}

// Routes the first argument to a command.
fn dispatch(ctx: &Ctx, cmd: &str, rest: &[String]) -> Result<Output, String> {
    match cmd {
        "find" | "grep" => find::run(ctx, rest),
        "read" | "cat" => read::run(ctx, rest),
        "ls" => ls::run(ctx, rest),
        "outline" => outline::run(ctx, rest),
        "refs" => refs::run(ctx, rest),
        "edit" => edit::run(ctx, rest),
        "clean" => clean::run(ctx, rest),
        "overview" => structure::overview(ctx, rest),
        "find-symbol" => structure::find_symbol(ctx, rest),
        "changed-since" => structure::changed_since(ctx, rest),
        "git-log" => structure::git_log(ctx, rest),
        "git-blame" => structure::git_blame(ctx, rest),
        "report-gap" => structure::report_gap(ctx, rest),
        "proxy" => {
            Err("proxy runs as its own process: start it with `zazzle proxy ..` (or `zazzled proxy ..`)"
                .into())
        }
        "run" => Err(
            "run executes in the caller's environment: use `zazzle run -- CMD ..` (or `zazzled wrap CMD ..`)"
                .into(),
        ),
        "status" => Ok(Output::plain(status(ctx))),
        "help" | "--help" | "-h" => Ok(Output::plain(HELP.into())),
        "version" | "--version" => Ok(Output::plain(format!("zazzle {VERSION}"))),
        other => Err(format!(
            "unknown command '{other}'; try: find read read-again ls outline refs edit clean overview find-symbol changed-since git-log git-blame report-gap status help"
        )),
    }
}

// Describes the daemon for the status command.
fn status(ctx: &Ctx) -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let e = ctx.engine;
    format!(
        "zazzled {VERSION} pid {} up {}s cache {} hot + {} stored ({} hits, {} misses); walks {} scanned, {} reused; {} sessions",
        std::process::id(),
        e.started.elapsed().as_secs(),
        e.cached_entries(),
        e.stored_records(),
        e.hits.load(Relaxed),
        e.misses.load(Relaxed),
        e.walks.scans.load(Relaxed),
        e.walks.reuses.load(Relaxed),
        e.sessions.count()
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::run_cmd;
    use crate::testutil::TempDir;

    // Checks help appears with no arguments and for the help words.
    #[test]
    fn test_help_variants() {
        let t = TempDir::new();
        for args in [vec![], vec!["help"], vec!["--help"], vec!["-h"]] {
            let o = run_cmd(t.path(), &args);
            assert!(o.text.starts_with("zazzle: file tool"), "{args:?}");
            assert_eq!(o.status, 0);
        }
    }

    // Checks version and status output.
    #[test]
    fn test_version_and_status() {
        let t = TempDir::new();
        assert!(run_cmd(t.path(), &["version"]).text.starts_with("zazzle 0."));
        assert!(run_cmd(t.path(), &["--version"]).text.starts_with("zazzle 0."));
        let s = run_cmd(t.path(), &["status"]).text;
        assert!(s.starts_with("zazzled 0.") && s.contains("cache 0 hot + 0 stored"));
    }

    // Checks the unknown command error is one line and lists commands.
    #[test]
    fn test_unknown_command() {
        let t = TempDir::new();
        let o = run_cmd(t.path(), &["frobnicate"]);
        assert_eq!(o.status, 2);
        assert_eq!(
            o.text,
            "error: unknown command 'frobnicate'; try: find read read-again ls outline refs edit clean overview find-symbol changed-since git-log git-blame report-gap status help"
        );
        assert!(!o.idable);
    }

    // Checks proxy is refused inside the daemon with a pointer to the right entry point.
    #[test]
    fn test_proxy_points_to_own_process() {
        let t = TempDir::new();
        let o = run_cmd(t.path(), &["proxy", "--", "srv"]);
        assert_eq!(o.status, 2);
        assert!(o.text.starts_with("error: proxy runs as its own process: start it with `zazzle proxy ..`"));
    }

    // Checks run is refused inside the daemon with a pointer to the right entry point.
    #[test]
    fn test_run_points_to_own_process() {
        let t = TempDir::new();
        assert!(run_cmd(t.path(), &["run", "--", "make"])
            .text
            .starts_with("error: run executes in the caller's environment"));
    }

    // Checks the aliases route to the same commands.
    #[test]
    fn test_aliases() {
        let t = TempDir::new();
        t.write("a.txt", b"hello\n");
        assert!(run_cmd(t.path(), &["grep", "hello"]).text.contains("a.txt"));
        assert!(run_cmd(t.path(), &["cat", "a.txt"]).text.contains("hello"));
    }

    // Checks stdin can be read once and then reports absence.
    #[test]
    fn test_read_stdin_once() {
        use super::*;
        let e = Engine::new();
        let ctx = Ctx {
            engine: &e,
            cwd: PathBuf::from("/"),
            session: String::new(),
            stdin: RefCell::new(Some(Box::new(&b"abc"[..]))),
        };
        assert_eq!(ctx.read_stdin().expect("stdin"), b"abc");
        assert_eq!(ctx.read_stdin().unwrap_err(), "no standard input available");
    }

    // Checks a failing stdin reader reports the error.
    #[test]
    fn test_read_stdin_error() {
        use super::*;
        struct Bad;
        impl Read for Bad {
            // Always fails.
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("boom"))
            }
        }
        let e = Engine::new();
        let ctx = Ctx {
            engine: &e,
            cwd: PathBuf::from("/"),
            session: String::new(),
            stdin: RefCell::new(Some(Box::new(Bad))),
        };
        assert_eq!(ctx.read_stdin().unwrap_err(), "reading stdin: boom");
    }

    // Converts string slices into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks global options are removed, --budget is parsed in both forms, and -- protects literals.
    #[test]
    fn test_split_globals() {
        let (rest, g) =
            split_globals("find", &v(&["x", "--disable-session-deduping", "--budget", "50", "-i"]))
                .expect("globals");
        assert_eq!((rest, g.dedupe, g.budget), (v(&["x", "-i"]), false, Some(50)));
        let (rest, g) = split_globals("ls", &v(&["--budget=7"])).expect("globals");
        assert_eq!((rest.len(), g.dedupe, g.budget), (0, true, Some(7)));
        let (rest, g) =
            split_globals("find", &v(&["--", "--budget", "--disable-session-deduping"])).expect("globals");
        assert_eq!((rest, g.budget), (v(&["--", "--budget", "--disable-session-deduping"]), None));
        let (rest, g) = split_globals("read", &v(&["f", "--budget", "9"])).expect("globals");
        assert_eq!((rest, g.budget), (v(&["f", "--budget", "9"]), None));
        assert_eq!(
            split_globals("find", &v(&["--budget"])).err().as_deref(),
            Some("--budget needs a number")
        );
        assert_eq!(
            split_globals("find", &v(&["--budget", "x"])).err().as_deref(),
            Some("--budget needs a number")
        );
        assert_eq!(
            split_globals("find", &v(&["--budget=0"])).err().as_deref(),
            Some("--budget must be at least 1")
        );
    }

    // Checks output is cut at a line boundary with a note, and short output is untouched.
    #[test]
    fn test_cut_to_budget() {
        let text: String = (0..100).map(|i| format!("line number {i}\n")).collect();
        let cut = cut_to_budget(text.clone(), 20);
        assert!(
            cut.starts_with("line number 0\nline number 1\n")
                && cut.contains("… cut at ~20 tokens (full output ~")
        );
        assert!(cut.lines().count() < 12);
        assert_eq!(cut_to_budget("short".into(), 20), "short");
        assert!(cut_to_budget("x".repeat(500), 5).starts_with("xxxx"));
    }

    // Checks descriptions are bounded.
    #[test]
    fn test_describe() {
        assert_eq!(describe("read", &v(&["a.rs:1-2"])), "zazzle read a.rs:1-2");
        assert_eq!(describe("status", &[]), "zazzle status");
        assert_eq!(describe("find", &["x".repeat(300)]).chars().count(), MAX_DESC);
    }

    // Checks the budget flag applies to non-read commands through the dispatcher.
    #[test]
    fn test_budget_applies_to_find() {
        let t = TempDir::new();
        let body: String = (0..300).map(|i| format!("needle {i}\n")).collect();
        t.write("a.txt", body.as_bytes());
        let cut = run_cmd(t.path(), &["find", "needle", "-m", "300", "--budget", "40"]).text;
        assert!(cut.contains("… cut at ~40 tokens") && cut.lines().count() < 20);
        assert_eq!(
            run_cmd(t.path(), &["find", "needle", "--budget", "0"]).text,
            "error: --budget must be at least 1"
        );
    }

    // Checks the session layer through the dispatcher: ids, notices, the opt-out flag, edits unaffected.
    #[test]
    fn test_session_dedupe_through_run() {
        let t = TempDir::new();
        t.write("a.txt", b"hello\n");
        let e = Engine::new();
        let go = |args: &[&str]| crate::testutil::run_cmd_with(&e, t.path(), "sess", args).text;
        let first = go(&["read", "a.txt"]);
        assert!(first.starts_with("[r:") && first.contains("1\thello"));
        let second = go(&["read", "a.txt"]);
        assert!(second
            .contains("already sent: zazzle read a.txt, unchanged. If you no longer have it: read-again r:"));
        assert_eq!(go(&["read", "a.txt", "--disable-session-deduping"]), first);
        let sha = first[first.find("sha ").expect("sha") + 4..][..8].to_string();
        let edited = go(&["edit", "a.txt", "--old", "hello", "--new", "bye", "--expect", &sha]);
        assert!(edited.starts_with("ok a.txt") && !edited.starts_with("[r:"));
        assert!(go(&["read", "a.txt"]).contains("1\tbye"));
        assert!(go(&["status"]).contains("1 sessions"));
        assert!(!run_cmd(t.path(), &["read", "a.txt"]).text.starts_with("[r:"));
    }
}
