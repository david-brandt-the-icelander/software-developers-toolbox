// Command dispatch: picks the subcommand, runs it and shapes failures for the wire.
use crate::engine::Engine;
use std::cell::RefCell;
use std::io::Read;
use std::path::PathBuf;

pub mod find;
pub mod ls;
pub mod read;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "afo: file tool for agents
find PATTERN [PATH..] [-i -F -w -c -l -u] [-C N] [-g GLOB] [-m N]  regex search grouped by file
read PATH[:A-B].. [--around N] [--ctx N] [--head N] [--tail N] [--budget T] [--anchors] [--full]
ls [DIR] [-d N] [-m N] [-u]  tree with line counts
status  daemon info
Any command: --disable-session-deduping resends output already sent in this session.";

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
        let Some(mut r) = self.stdin.borrow_mut().take() else {
            return Err("no standard input available".into());
        };
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).map_err(|e| format!("reading stdin: {e}"))?;
        Ok(buf)
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

// Runs one command line and returns its output.
pub fn run(ctx: &Ctx, args: &[String]) -> Output {
    match dispatch(ctx, args) {
        Ok(o) => o,
        Err(msg) => Output::err(&msg),
    }
}

// Routes the first argument to a command.
fn dispatch(ctx: &Ctx, args: &[String]) -> Result<Output, String> {
    let Some((cmd, rest)) = args.split_first() else { return Ok(Output::plain(HELP.into())) };
    match cmd.as_str() {
        "find" | "grep" => find::run(ctx, rest),
        "read" | "cat" => read::run(ctx, rest),
        "ls" => ls::run(ctx, rest),
        "status" => Ok(Output::plain(status(ctx))),
        "help" | "--help" | "-h" => Ok(Output::plain(HELP.into())),
        "version" | "--version" => Ok(Output::plain(format!("afo {VERSION}"))),
        other => Err(format!("unknown command '{other}'; try: find read ls status help")),
    }
}

// Describes the daemon for the status command.
fn status(ctx: &Ctx) -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let e = ctx.engine;
    format!(
        "afod {VERSION} pid {} up {}s cache {} entries ({} hits, {} misses)",
        std::process::id(),
        e.started.elapsed().as_secs(),
        e.cached_entries(),
        e.hits.load(Relaxed),
        e.misses.load(Relaxed)
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::testutil::run_cmd;
    use crate::testutil::TempDir;

    // Checks help appears with no arguments and for the help words.
    #[test]
    fn test_help_variants() {
        let t = TempDir::new();
        for args in [vec![], vec!["help"], vec!["--help"], vec!["-h"]] {
            let o = run_cmd(t.path(), &args);
            assert!(o.text.starts_with("afo: file tool"), "{args:?}");
            assert_eq!(o.status, 0);
        }
    }

    // Checks version and status output.
    #[test]
    fn test_version_and_status() {
        let t = TempDir::new();
        assert!(run_cmd(t.path(), &["version"]).text.starts_with("afo 0."));
        assert!(run_cmd(t.path(), &["--version"]).text.starts_with("afo 0."));
        let s = run_cmd(t.path(), &["status"]).text;
        assert!(s.starts_with("afod 0.") && s.contains("cache 0 entries"));
    }

    // Checks the unknown command error is one line and lists commands.
    #[test]
    fn test_unknown_command() {
        let t = TempDir::new();
        let o = run_cmd(t.path(), &["frobnicate"]);
        assert_eq!(o.status, 2);
        assert_eq!(o.text, "error: unknown command 'frobnicate'; try: find read ls status help");
        assert!(!o.idable);
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
}
