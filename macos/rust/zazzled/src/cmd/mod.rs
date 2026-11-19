// Command dispatch: picks the subcommand, runs it and shapes failures for the wire.
use crate::engine::Engine;
use crate::session::Replay;
use crate::tokens;
use std::cell::RefCell;
use std::io::Read;
use std::path::PathBuf;

pub mod again;
mod align;
pub mod clean;
pub mod diff;
pub mod edit;
pub mod find;
pub mod help;
pub mod languages;
pub mod ls;
pub mod outline;
mod pin;
pub mod read;
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod read_regression;
pub mod refs;
pub mod replace;
pub mod structure;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_BUDGET: usize = 3000;
// A brief carries whole skills, so it gets a larger default budget.
const BRIEF_BUDGET: usize = 8000;
const MAX_DESC: usize = 100;

// The invoker's own standard streams and connection, for commands that hand them to a script.
pub struct Caller {
    pub stdio: [std::os::fd::OwnedFd; 3],
    pub conn: Option<std::os::fd::RawFd>,
}

// Where a command runs and who asked.
pub struct Ctx<'a> {
    pub engine: &'a Engine,
    pub cwd: PathBuf,
    pub session: String,
    pub roots: Vec<PathBuf>,
    pub stdin: RefCell<Option<Box<dyn Read>>>,
    pub caller: RefCell<Option<Caller>>,
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
    pub raw: bool,
    // Text for the caller, not for whatever reads standard output (the rules that apply to what was read): standard error on the command line, appended over MCP.
    pub notes: String,
}

impl Output {
    // A successful result whose text may be deduplicated within a session.
    pub fn ok(text: String) -> Output {
        Output { text, status: 0, idable: true, raw: false, notes: String::new() }
    }

    // A successful result that must always be sent in full.
    pub fn plain(text: String) -> Output {
        Output { text, status: 0, idable: false, raw: false, notes: String::new() }
    }

    // A failure with a one-line message.
    pub fn err(msg: &str) -> Output {
        Output { text: format!("error: {msg}"), status: 2, idable: false, raw: false, notes: String::new() }
    }

    // Text written to standard output exactly as it is (no newline added), with any exit status.
    pub fn raw(text: String, status: u8) -> Output {
        Output { text, status, idable: false, raw: true, notes: String::new() }
    }
}

// Options every command accepts, removed from the command line before dispatch.
struct Globals {
    dedupe: bool,
    budget: Option<usize>,
    commit: Option<String>,
    llm: Option<String>,
}

// Pulls the global options out of a command's arguments (`--llm INSTRUCTION` among them); read keeps its own --budget.
fn split_globals(cmd: &str, args: &[String]) -> Result<(Vec<String>, Globals), String> {
    let takes_budget = !matches!(cmd, "read" | "cat" | "clean" | "diff");
    let (mut rest, mut g) = (Vec::new(), Globals { dedupe: true, budget: None, commit: None, llm: None });
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            rest.push(a.clone());
            rest.extend(it.by_ref().cloned());
        } else if a == "--disable-session-deduping" {
            g.dedupe = false;
        } else if a == "--commit" || a.starts_with("--commit=") {
            let value = a.strip_prefix("--commit=").map(str::to_string).or_else(|| it.next().cloned());
            g.commit = Some(value.filter(|v| !v.is_empty()).ok_or("--commit needs a commit sha (or HEAD)")?);
        } else if a == "--llm" || a.starts_with("--llm=") {
            let value = a.strip_prefix("--llm=").map(str::to_string).or_else(|| it.next().cloned());
            g.llm = Some(
                value
                    .filter(|v| !v.trim().is_empty())
                    .ok_or("--llm needs an instruction for the local model")?,
            );
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
    let (rest, pinned) = match globals.commit.as_deref().map(|c| pin::apply(ctx, cmd, &rest, c)) {
        None => (rest, String::new()),
        Some(Ok(x)) => x,
        Some(Err(e)) => return (Output::err(&e), String::new(), true),
    };
    let mut out = match dispatch(ctx, cmd, &rest) {
        Ok(o) => o,
        Err(msg) => Output::err(&msg),
    };
    if let Some(instruction) = globals.llm.as_deref().filter(|_| out.status == 0) {
        out = match crate::llm::post(&out.text, instruction) {
            Ok(text) => Output { text, ..out },
            Err(e) => Output::err(&e),
        };
    }
    if out.idable && !matches!(cmd, "read" | "cat" | "clean" | "diff") {
        let brief = cmd == "brief" || rest.first().is_some_and(|c| c == "brief");
        let default = if brief { BRIEF_BUDGET } else { DEFAULT_BUDGET };
        out.text = cut_to_budget(out.text, globals.budget.unwrap_or(default));
    }
    if !pinned.is_empty() {
        out.text = format!("{}\n{pinned}", out.text.trim_end());
    }
    (out, describe(cmd, &rest), globals.dedupe)
}

// Runs one command line and returns its output, tagged or deduplicated for the caller's session, remembering how to run it again.
pub fn run(ctx: &Ctx, args: &[String]) -> Output {
    let Some((cmd, rest)) = args.split_first() else { return Output::plain(help::overview()) };
    if let Some(text) = command_help(cmd, rest) {
        return Output::plain(text);
    }
    if cmd == "read-again" {
        return again::run(ctx, rest).unwrap_or_else(|e| Output::err(&e));
    }
    let (mut out, desc, dedupe) = produce(ctx, cmd, rest);
    if out.status != 0 && !out.raw {
        out.text = shorten_repeats(ctx, &out.text);
    }
    let pre_dedupe_bytes = out.text.len();
    let mut deduplicated = false;
    if out.idable {
        let kept: Vec<String> = args.iter().filter(|a| *a != "--disable-session-deduping").cloned().collect();
        let replay = Replay { args: kept, cwd: ctx.cwd.clone() };
        let applied = ctx.engine.sessions.apply_tracked(&ctx.session, &desc, Some(replay), out.text, dedupe);
        out.text = applied.text;
        deduplicated = applied.deduplicated;
    }
    if out.status == 0 && !ctx.session.is_empty() && matches!(cmd.as_str(), "read" | "cat" | "edit") {
        let (has, mark) = ledger(ctx);
        let l = crate::rules::Ledger { has: &has, mark: &mark };
        let purposes = ctx.engine.sessions.purposes(&ctx.session);
        out.notes.push_str(&crate::nested::for_paths(&ctx.cwd, &touched(&ctx.cwd, rest), &purposes, &l));
    }
    let shown = format!("{}{}", out.text, out.notes);
    ctx.engine.deliveries.record(&ctx.session, cmd, pre_dedupe_bytes, &shown, out.status, deduplicated);
    out
}

// Returns the session's rule ledger as a lookup and a record, shared by get-rules and the instructions appended to reads and edits.
fn ledger<'a>(ctx: &'a Ctx) -> (impl Fn(&str) -> bool + 'a, impl Fn(&str) + 'a) {
    (
        move |k: &str| ctx.engine.sessions.has_note(&ctx.session, k),
        move |k: &str| ctx.engine.sessions.mark_note(&ctx.session, k),
    )
}

// Runs get-rules against the session's ledger, or the zazzle-agent command line for anything else under `agent`.
fn get_rules(ctx: &Ctx, rest: &[String]) -> Result<Output, String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let (has, mark) = ledger(ctx);
    let l = crate::rules::Ledger { has: &has, mark: &mark };
    let l = (!ctx.session.is_empty()).then_some(&l);
    if let Some(p) = crate::rules::purposes(rest).filter(|_| !ctx.session.is_empty()) {
        ctx.engine.sessions.set_purposes(&ctx.session, p);
    }
    crate::rules::get_rules(rest, &ctx.cwd, home.as_deref(), l).map(Output::plain)
}

// Runs brief: the rule sections and skills the local model picks for an intent.
fn brief(ctx: &Ctx, rest: &[String]) -> Result<Output, String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    crate::brief::run(rest, &ctx.cwd, home.as_deref(), &crate::brief::ask_local).map(Output::ok)
}

// Answers `agent prompt [--effort E] TEXT..` in the daemon (for MCP): the local model's whole answer, never downloading a model.
fn prompt(rest: &[String]) -> Result<Output, String> {
    const USAGE: &str =
        "usage: agent prompt [--effort low|medium|high] [--max-tokens N] [--system TEXT] [--] TEXT..";
    let (mut flags, mut words, mut it) = (Vec::<&str>::new(), Vec::new(), rest.iter());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => words.extend(it.by_ref().map(String::as_str)),
            f @ ("--effort" | "--max-tokens" | "--system") => {
                flags.extend([f, it.next().ok_or_else(|| format!("{f} needs a value; {USAGE}"))?.as_str()])
            }
            f if f.starts_with("--") => return Err(format!("unknown flag {f}; {USAGE}")),
            w => words.push(w),
        }
    }
    let text = words.join(" ");
    if text.trim().is_empty() {
        return Err(USAGE.into());
    }
    let reply = crate::llm::ask(&text, "", &flags)?;
    let cut = if reply.truncated { "\n[the token limit cut this answer short]" } else { "" };
    Ok(Output::plain(format!("{}{cut}", reply.text)))
}

// Returns the existing files a read or edit names, without line ranges.
fn touched(cwd: &std::path::Path, rest: &[String]) -> Vec<std::path::PathBuf> {
    rest.iter()
        .filter(|a| !a.starts_with('-'))
        .map(|a| match a.rsplit_once(':') {
            Some((p, r))
                if !p.is_empty() && r.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ',') =>
            {
                p
            }
            _ => a.as_str(),
        })
        .map(|p| cwd.join(p))
        .filter(|p| p.is_file())
        .collect()
}

// Fixed parts of error messages, keyed by the text matched without digits (so a failure's age does not count): each is sent in full once per session, then shortened when that is shorter.
const REPEATS: [(&str, &str); 3] = [
    (r"usage: [^\n]*", "usage: as above"),
    (r"; similar: [^\n]*", "; similar: as listed earlier"),
    (
        r"C# indexer unavailable: [^\n]*",
        "C# indexer still unavailable, as reported earlier; other tools work without it.",
    ),
];

// Shortens the fixed parts of an error the session was already sent: usage lines, the indexer notes and repeated `similar:` lists.
fn shorten_repeats(ctx: &Ctx, text: &str) -> String {
    static RES: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    let res =
        RES.get_or_init(|| REPEATS.iter().map(|(r, _)| regex::Regex::new(r).expect("valid regex")).collect());
    let mut out = text.to_string();
    let sessions = &ctx.engine.sessions;
    for (re, (_, short)) in res.iter().zip(REPEATS) {
        if let Some(m) = re.find(&out).map(|m| m.as_str().to_string()).filter(|m| m.len() > short.len()) {
            let key: String = m.chars().filter(|c| !c.is_ascii_digit()).collect();
            if !sessions.first_note(&ctx.session, &key) {
                out = out.replacen(&m, short, 1);
            }
        }
    }
    out
}

// Returns a command's help page when its first argument is --help or -h (zazz answers its own).
fn command_help(cmd: &str, rest: &[String]) -> Option<String> {
    if cmd == "zazz" || !matches!(rest.first().map(String::as_str), Some("--help" | "-h")) {
        return None;
    }
    help::page(match cmd {
        "grep" => "find",
        "cat" => "read",
        other => other,
    })
    .ok()
}

// Routes the first argument to a command.
fn dispatch(ctx: &Ctx, cmd: &str, rest: &[String]) -> Result<Output, String> {
    let aligned = align::args(cmd, rest);
    let rest = aligned.as_slice();
    match cmd {
        "find" | "grep" => find::run(ctx, rest),
        "read" | "cat" => read::run(ctx, rest),
        "ls" => ls::run(ctx, rest),
        "outline" => outline::run(ctx, rest),
        "refs" => refs::run(ctx, rest),
        "diff" => diff::run(ctx, rest),
        "replace" => replace::run(ctx, rest),
        "edit" => edit::run(ctx, rest),
        "clean" => clean::run(ctx, rest),
        "overview" => structure::overview(ctx, rest),
        "find-symbol" => structure::find_symbol(ctx, rest),
        "changed-since" => structure::changed_since(ctx, rest),
        "git-log" => structure::git_log(ctx, rest),
        "git-blame" => structure::git_blame(ctx, rest),
        "report-gap" => structure::report_gap(ctx, rest),
        "proxy" | "setup" => Err(format!(
            "{cmd} runs as its own process: start it with `zazzle {cmd} ..` (or `zazzled {cmd} ..`)"
        )),
        "run" => Err(
            "run executes in the caller's environment: use `zazzle run -- CMD ..` (or `zazzled wrap CMD ..`)"
                .into(),
        ),
        "zazz" => crate::zazz::run(ctx, rest),
        "get-rules" => get_rules(ctx, rest),
        "brief" => brief(ctx, rest),
        "prompt" => prompt(rest),
        "agent" => match rest.split_first() {
            Some((c, r)) if c == "get-rules" && !r.iter().any(|a| a == "--help" || a == "-h") => {
                get_rules(ctx, r)
            }
            Some((c, r)) if c == "brief" => brief(ctx, r),
            Some((c, r)) if c == "prompt" => prompt(r),
            Some((c, _)) if c == "llm" => Err(
                "agent llm runs as its own process: start it with `zazzle agent llm ..` (or `zazzled llm llm ..`)".into(),
            ),
            _ => {
                let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
                let (text, code) =
                    zazzle_agent_rules_list::run(rest, home.as_deref(), std::path::Path::new("/"));
                if code == 0 {
                    Ok(Output::plain(text.trim_end().to_string()))
                } else {
                    Err(text.trim_end().to_string())
                }
            }
        },
        "forget-sessions" => match rest {
            [prefix] if !prefix.is_empty() => {
                Ok(Output::plain(format!("forgot {} sessions", ctx.engine.sessions.forget_prefix(prefix))))
            }
            _ => Err("usage: zazzle forget-sessions PREFIX".into()),
        },
        "status" => Ok(Output::plain(status(ctx))),
        "trace" => crate::proxy::trace::command(rest, &ctx.cwd).map(Output::plain),
        "languages" => languages::run(ctx, rest),
        "help" | "--help" | "-h" => match rest {
            [] => Ok(Output::plain(help::overview())),
            [a] if a == "--all" => Ok(Output::plain(help::overview())),
            [a] if a == "--agent" => Ok(Output::plain(help::agent_overview())),
            [name] => help::page(name).map(Output::plain),
            _ => Err("usage: zazzle help [COMMAND]".into()),
        },
        "version" | "--version" => Ok(Output::plain(format!("zazzle {VERSION}"))),
        other => Err(format!("unknown command '{other}'; try: {}", help::AGENT.join(" "))),
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

    // Checks help --all lists every command and help --agent only the agent commands.
    #[test]
    fn test_help_all_and_agent() {
        let t = crate::testutil::TempDir::new();
        assert!(run_cmd(t.path(), &["help", "--all"]).text.contains("setup"));
        assert!(run_cmd(t.path(), &["help", "--agent"]).text.ends_with("humans: zazzle help --all"));
        assert!(run_cmd(t.path(), &["forget-sessions", ""]).text.contains("usage: zazzle forget-sessions"));
    }

    // Checks forget-sessions drops the sessions under a prefix and needs one.
    #[test]
    fn test_forget_sessions() {
        let t = crate::testutil::TempDir::new();
        t.write("a.txt", b"x\n");
        let e = crate::engine::Engine::new();
        crate::testutil::run_cmd_with(&e, t.path(), "S/main", &["read", "a.txt"]);
        let o = crate::testutil::run_cmd_with(&e, t.path(), "", &["forget-sessions", "S/"]);
        assert_eq!(o.text, "forgot 1 sessions");
        assert!(run_cmd(t.path(), &["forget-sessions"])
            .text
            .contains("usage: zazzle forget-sessions PREFIX"));
    }

    // Checks help appears with no arguments and for the help words.
    #[test]
    fn test_help_variants() {
        let t0 = crate::testutil::TempDir::new();
        let all = run_cmd(t0.path(), &["help", "--all"]).text;
        assert!(all.contains("setup") && run_cmd(t0.path(), &["help"]).text == all);
        let agent = run_cmd(t0.path(), &["help", "--agent"]).text;
        assert!(agent.ends_with("humans: zazzle help --all") && !agent.contains("setup"));
        assert_eq!(
            touched(t0.path(), &["a:".into(), "b:x".into(), ":1".into()]),
            Vec::<std::path::PathBuf>::new()
        );
        t0.write("f:x", b"f");
        assert_eq!(touched(t0.path(), &["f:x".into(), "--source".into()]), vec![t0.path().join("f:x")]);

        let t = TempDir::new();
        for args in [vec![], vec!["help"], vec!["--help"], vec!["-h"]] {
            let o = run_cmd(t.path(), &args);
            assert!(o.text.starts_with(&format!("zazzle {VERSION}: read, search and edit")), "{args:?}");
            assert_eq!(o.status, 0);
        }
        let page = run_cmd(t.path(), &["help", "find"]).text;
        assert!(page.starts_with("zazzle find: Search files"));
        for args in [["find", "--help"], ["grep", "-h"], ["read-again", "--help"], ["ls", "-h"]] {
            let o = run_cmd(t.path(), &args);
            assert!(o.text.starts_with("zazzle "), "{args:?}");
            assert!(o.text.contains("\nUsage:\n"), "{args:?}");
        }
        assert_eq!(run_cmd(t.path(), &["find", "--help"]).text, page);
        let o = run_cmd(t.path(), &["help", "nope"]);
        assert_eq!(
            (o.text.as_str(), o.status),
            ("error: no command 'nope'; known: read outline find refs find-symbol edit replace ls diff run clean read-again get-rules brief prompt", 2)
        );
        assert_eq!(run_cmd(t.path(), &["help", "a", "b"]).text, "error: usage: zazzle help [COMMAND]");
        assert!(run_cmd(t.path(), &["frobnicate", "--help"]).text.starts_with("error: unknown command"));
    }

    // Checks version and status output.
    #[test]
    fn test_version_and_status() {
        let t = TempDir::new();
        let version = env!("CARGO_PKG_VERSION");
        assert!(run_cmd(t.path(), &["version"]).text.starts_with(&format!("zazzle {version}")));
        assert!(run_cmd(t.path(), &["--version"]).text.starts_with(&format!("zazzle {version}")));
        let s = run_cmd(t.path(), &["status"]).text;
        assert!(s.starts_with(&format!("zazzled {version}")) && s.contains("cache 0 hot + 0 stored"));
    }

    // Checks the unknown command error is one line and lists commands.
    #[test]
    fn test_unknown_command() {
        let t = TempDir::new();
        let o = run_cmd(t.path(), &["frobnicate"]);
        assert_eq!(o.status, 2);
        assert_eq!(
            o.text,
            "error: unknown command 'frobnicate'; try: read outline find refs find-symbol edit replace ls diff run clean read-again get-rules brief prompt"
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
        assert!(run_cmd(t.path(), &["setup"]).text.starts_with("error: setup runs as its own process"));
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
            roots: Vec::new(),
            engine: &e,
            cwd: PathBuf::from("/"),
            session: String::new(),
            stdin: RefCell::new(Some(Box::new(&b"abc"[..]))),
            caller: RefCell::new(None),
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
            roots: Vec::new(),
            engine: &e,
            cwd: PathBuf::from("/"),
            session: String::new(),
            stdin: RefCell::new(Some(Box::new(Bad))),
            caller: RefCell::new(None),
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
        let (rest, g) = split_globals("read", &v(&["f", "--llm", "Only the imports"])).expect("llm");
        assert_eq!((rest, g.llm.as_deref()), (v(&["f"]), Some("Only the imports")));
        assert_eq!(
            split_globals("ls", &v(&["--llm=sum up"])).expect("llm=").1.llm.as_deref(),
            Some("sum up")
        );
        assert!(split_globals("ls", &v(&["--llm", " "]))
            .err()
            .expect("blank")
            .contains("needs an instruction"));
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

    // Checks --llm hands a command's output to the local model and a failure of the model becomes the command's error.
    #[test]
    fn test_llm_post_processes_output() {
        let _g = crate::testutil::env_lock();
        let t = TempDir::new();
        let long: String = (0..40)
            .map(|i| format!("needle {i} in a line long enough to make the output worth summarizing\n"))
            .collect();
        t.write("a.txt", long.as_bytes());
        t.write("short.txt", b"needle one\n");
        let llm = t.write("fake-llm", b"#!/bin/sh\ncat >/dev/null; echo two matches\n");
        std::fs::set_permissions(&llm, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        std::env::set_var("ZAZZLE_LLM", &llm);
        let out = run_cmd(t.path(), &["find", "needle", "a.txt", "--llm", "Count the matches"]);
        assert!(out.text.starts_with("two matches\n[post-processed by the local model: ~"), "{}", out.text);
        let short = run_cmd(t.path(), &["find", "needle", "short.txt", "--llm", "Count the matches"]);
        assert!(
            short.text.starts_with("short.txt\n1: needle one") && short.text.contains("[kept as is: ~"),
            "{}",
            short.text
        );
        let bad = t.write("bad-llm", b"#!/bin/sh\necho 'error: no model' >&2; exit 4\n");
        std::fs::set_permissions(&bad, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        std::env::set_var("ZAZZLE_LLM", &bad);
        let failed = run_cmd(t.path(), &["find", "needle", "a.txt", "--llm", "Count"]);
        std::env::remove_var("ZAZZLE_LLM");
        assert_eq!((failed.status, failed.text.as_str()), (2, "error: local model: no model"));
    }

    // Checks prompt and brief reach the local model through the dispatcher (also under agent), and their usage and cut-short notes.
    #[test]
    fn test_prompt_and_brief_dispatch() {
        let _g = crate::testutil::env_lock();
        let t = TempDir::new();
        t.write("AGENTS.md", b"# Rules\nBe kind.\n");
        let fake = t.write("fake-llm", b"#!/bin/sh\nfor a; do last=$a; done\ncase \"$*\" in *grammar*) seq -f %g:y -s \" \" 99;; *cut*) echo half; exit 3;; *) echo \"$last\";; esac\n");
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        std::env::set_var("ZAZZLE_LLM", &fake);
        assert_eq!(run_cmd(t.path(), &["prompt", "--effort", "low", "Hi", "there"]).text, "Hi there");
        assert_eq!(
            run_cmd(t.path(), &["agent", "prompt", "cut"]).text,
            "half\n[the token limit cut this answer short]"
        );
        assert!(run_cmd(t.path(), &["prompt", "--effort"]).text.contains("--effort needs"));
        assert_eq!(
            run_cmd(t.path(), &["prompt", "--max-tokens", "50", "--system", "s", "--", "--x", "y"]).text,
            "--x y"
        );
        assert!(run_cmd(t.path(), &["prompt", "--model", "m", "hi"]).text.contains("unknown flag --model"));
        assert!(run_cmd(t.path(), &["prompt"]).text.contains("usage: agent prompt"));
        let b = run_cmd(t.path(), &["agent", "brief", "be", "nice"]).text;
        std::env::remove_var("ZAZZLE_LLM");
        assert!(b.contains("==> rule AGENTS.md:1-2 <==\n# Rules\nBe kind."), "{b}");
        assert!(run_cmd(t.path(), &["agent", "llm"]).text.contains("runs as its own process"));
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

    // Checks an error's usage line and similar-file list are sent in full once per session, then shortened, while other sessions still get them.
    #[test]
    fn test_repeated_error_parts_shortened() {
        let t = TempDir::new();
        t.write("d1/notes.md", b"a\n");
        t.write("d2/notes.md", b"a\n");
        let e = Engine::new();
        let go = |s: &str, args: &[&str]| crate::testutil::run_cmd_with(&e, t.path(), s, args).text;
        let usage = go("s", &["outline", "--zzz"]);
        assert!(usage.contains("usage: outline PATH.."), "{usage}");
        assert_eq!(
            go("s", &["outline", "--zzz"]),
            "error: unknown flag --zzz; known: --depth --max; usage: as above"
        );
        assert_eq!(go("t", &["outline", "--zzz"]), usage);
        let missing = go("s", &["read", "x/notes.md"]);
        assert!(missing.ends_with("similar: d1/notes.md, d2/notes.md"), "{missing}");
        assert!(go("s", &["read", "x/notes.md"]).ends_with("; similar: as listed earlier"));
        let short = go("s", &["read", "nope.md"]);
        assert_eq!(short, "error: nope.md: no such file or directory");
        let note =
            "C# indexer unavailable: it is gone (3s ago). find, read, ls, outline and refs work without it.";
        let ctx = |s: &str| Ctx {
            roots: Vec::new(),
            engine: &e,
            cwd: t.path().to_path_buf(),
            session: s.into(),
            stdin: RefCell::new(None),
            caller: RefCell::new(None),
        };
        assert_eq!(shorten_repeats(&ctx("s"), note), note);
        let again = shorten_repeats(&ctx("s"), &note.replace("3s", "9s"));
        assert_eq!(again, "C# indexer still unavailable, as reported earlier; other tools work without it.");
    }
}
