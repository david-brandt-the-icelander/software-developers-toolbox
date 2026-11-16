// `zazzle git ARGS`: answers help and version itself, runs `message` and `commit`, and hands everything else to git unchanged.

use crate::git::{exec_git, git};
use crate::{commit, message, offer};

// The help text for `zazzle git`.
pub const HELP: &str = "usage: zazzle git [message FILE | commit [--no-add] [OPTIONS] | GIT ARGS...]

  zazzle git message FILE   edit a commit message (or other short text) in a small editor
  zazzle git commit         draft the message, choose the files, then commit
  zazzle git help COMMAND   help for message or commit; other topics go to `git help`

Everything else is passed to git unchanged, for example `zazzle git status -sb`.
Git's own help: git help, or zazzle git help TOPIC.
";

// What a command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Help,
    MessageHelp,
    CommitHelp,
    Version,
    Message(Vec<String>),
    Commit(Vec<String>),
    Git(Vec<String>),
}

// Decides what to do with the arguments after `zazzle git`.
pub fn route(args: &[String]) -> Route {
    let first = args.first().map(String::as_str);
    let rest = args.get(1..).unwrap_or(&[]).to_vec();
    match first {
        None | Some("-h" | "--help") => Route::Help,
        Some("help") => match rest.first().map(String::as_str) {
            None => Route::Help,
            Some("message") => Route::MessageHelp,
            Some("commit") => Route::CommitHelp,
            Some(_) => Route::Git(args.to_vec()),
        },
        Some("-v" | "--version" | "version") => Route::Version,
        Some("message") => Route::Message(rest),
        Some("commit") => Route::Commit(rest),
        Some(_) => Route::Git(args.to_vec()),
    }
}

// Returns the version lines: this tool's, then git's.
pub fn version() -> String {
    let git_version = git(&["--version"]).unwrap_or_else(|e| format!("git: {e}\n"));
    format!("zazzle-git {}\n{git_version}", env!("CARGO_PKG_VERSION"))
}

// Runs `zazzle git ARGS` and returns the exit code (git's own when forwarding).
pub fn main(args: &[String]) -> i32 {
    match route(args) {
        Route::Help => print!("{HELP}\n{}", offer::HOW_TO),
        Route::MessageHelp => print!("{}\n{}", message::HELP, offer::HOW_TO),
        Route::CommitHelp => print!("{}\n{}", commit::HELP, offer::HOW_TO),
        Route::Version => print!("{}", version()),
        Route::Message(rest) => return message::main(&rest),
        Route::Commit(rest) => return commit::main(&rest),
        Route::Git(all) => return exec_git(&all),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    // Turns words into arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks every route, including help topics that go to git.
    #[test]
    fn test_route() {
        assert_eq!(route(&[]), Route::Help);
        assert_eq!(route(&v(&["-h"])), Route::Help);
        assert_eq!(route(&v(&["--help"])), Route::Help);
        assert_eq!(route(&v(&["help"])), Route::Help);
        assert_eq!(route(&v(&["help", "message"])), Route::MessageHelp);
        assert_eq!(route(&v(&["help", "commit"])), Route::CommitHelp);
        assert_eq!(route(&v(&["help", "rebase"])), Route::Git(v(&["help", "rebase"])));
        assert_eq!(route(&v(&["-v"])), Route::Version);
        assert_eq!(route(&v(&["--version"])), Route::Version);
        assert_eq!(route(&v(&["version"])), Route::Version);
        assert_eq!(route(&v(&["message", "f"])), Route::Message(v(&["f"])));
        assert_eq!(route(&v(&["commit", "--no-add"])), Route::Commit(v(&["--no-add"])));
        assert_eq!(route(&v(&["status", "-sb"])), Route::Git(v(&["status", "-sb"])));
    }

    // Checks the version text and the routes that print and return 0.
    #[test]
    fn test_main_prints() {
        assert!(version().starts_with("zazzle-git "));
        assert!(version().contains("git version"));
        for a in [v(&[]), v(&["help", "message"]), v(&["help", "commit"]), v(&["--version"])] {
            assert_eq!(main(&a), 0);
        }
        assert_eq!(main(&v(&["commit", "-h"])), 0);
        assert_eq!(main(&v(&["message", "-h"])), 0);
    }
}
