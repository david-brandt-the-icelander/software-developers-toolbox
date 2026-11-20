// `zazzle git amend` and `zazzle git undo`: rewrite the last commit's message in the editor (adding what is staged), or take the last commit back keeping its changes.

use crate::commit::git_path;
use crate::editor::Outcome;
use crate::git::{git, git_interactive};
use crate::message;
use std::fs;

// The help text for `zazzle git amend`.
pub const AMEND_HELP: &str = "usage: zazzle git amend [--no-edit]

Opens the last commit's message in the commit message editor, then amends the
commit with the edited message; staged changes are added to it, like
`git commit --amend`. Ctrl+S amends, Esc cancels and leaves the commit as it is.
  --no-edit   add the staged changes and keep the message, without the editor
";

// The help text for `zazzle git undo`.
pub const UNDO_HELP: &str = "usage: zazzle git undo [--force]

Takes back the last commit and keeps its changes staged, like
`git reset --soft HEAD~1`, and prints the short hash and subject it undid.
It refuses when that commit is already on the upstream branch, because undoing
a pushed commit rewrites shared history.
  --force   undo a pushed commit anyway
";

// The message file amend edits inside the git directory; the editor treats it as a commit message.
const AMEND_MSG: &str = "ZAZZLE_AMEND_MSG";

// Reads a command line that takes one flag: None for help, else whether the flag is given; anything else is an error.
pub fn parse_flag(args: &[String], flag: &str) -> Result<Option<bool>, String> {
    let mut on = false;
    for a in args {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            s if s == flag => on = true,
            s => return Err(format!("unknown argument {s}")),
        }
    }
    Ok(Some(on))
}

// Returns the text the amend editor opens: the last message, then guidance and the staged changes as comments.
pub fn amend_draft(last: &str, comment: char, staged: &str) -> String {
    let c = comment;
    let mut s = format!("{}\n\n", last.trim_end());
    s.push_str(&format!("{c} Amending the last commit; lines starting with '{c}' are ignored.\n"));
    s.push_str(&format!("{c} Ctrl+S amends, Esc cancels and leaves the commit as it is.\n"));
    if staged.trim().is_empty() {
        s.push_str(&format!("{c} Nothing is staged: only the message changes.\n"));
        return s;
    }
    s.push_str(&format!("{c} Staged changes that will be added to it:\n"));
    for line in staged.lines() {
        s.push_str(&format!("{c}   {}\n", line.replace('\t', " ")));
    }
    s
}

// Fails unless the working directory is in a git repository that has a commit.
fn need_commit(what: &str) -> Result<(), String> {
    git(&["rev-parse", "--git-dir"])?;
    match git(&["rev-parse", "-q", "--verify", "HEAD"]) {
        Ok(_) => Ok(()),
        Err(_) => Err(format!("there is no commit to {what} yet")),
    }
}

// Amends the last commit, editing its message unless --no-edit, and returns git's exit code, or 1 when cancelled.
fn amend(no_edit: bool) -> Result<i32, String> {
    need_commit("amend")?;
    if no_edit {
        return Ok(git_interactive(&["commit".into(), "--amend".into(), "--no-edit".into()]));
    }
    let file = git_path(AMEND_MSG)?;
    let draft = amend_draft(
        &git(&["log", "-1", "--format=%B"])?,
        message::comment_char(),
        &git(&["diff", "--cached", "--name-status"])?,
    );
    fs::write(&file, draft).map_err(|e| format!("{}: {e}", file.display()))?;
    let edited = message::edit(&file, Vec::new());
    if !matches!(edited, Ok(ref e) if e.outcome == Outcome::Save) {
        let _ = fs::remove_file(&file);
        return edited.map(|_| 1);
    }
    let path = file.to_string_lossy().into_owned();
    let code = git_interactive(&["commit", "--amend", "--cleanup=strip", "-F", &path].map(String::from));
    if code == 0 {
        let _ = fs::remove_file(&file);
    } else {
        eprintln!("zazzle git amend: git commit failed; your message is in {path}");
    }
    Ok(code)
}

// Returns the upstream branch when the last commit is already on it, or None when there is no upstream or it is not.
fn pushed_to() -> Option<String> {
    let upstream = git(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).ok()?;
    git(&["merge-base", "--is-ancestor", "HEAD", "@{upstream}"]).ok()?;
    Some(upstream.trim().to_string())
}

// Takes back the last commit keeping its changes staged and returns what it undid; refuses a root commit, and a pushed one without force.
fn undo(force: bool) -> Result<String, String> {
    need_commit("undo")?;
    let summary = git(&["log", "-1", "--format=%h %s"])?.trim().to_string();
    if git(&["rev-list", "--parents", "-n", "1", "HEAD"])?.split_whitespace().count() < 2 {
        return Err(format!(
            "{summary} is the first commit, so there is nothing to go back to; \
             to keep its changes staged with no commit, run `git update-ref -d HEAD`"
        ));
    }
    if let Some(upstream) = pushed_to().filter(|_| !force) {
        return Err(format!(
            "{summary} is already on {upstream}; undoing it rewrites shared history (--force undoes it anyway)"
        ));
    }
    git(&["reset", "-q", "--soft", "HEAD~1"])?;
    Ok(format!("undid {summary}; its changes are staged\n"))
}

// Runs a one-flag command: prints its help, runs it, or reports a usage error (exit 2) or a failure (exit 1).
fn command(
    name: &str,
    help: &str,
    flag: &str,
    args: &[String],
    run: impl FnOnce(bool) -> Result<i32, String>,
) -> i32 {
    match parse_flag(args, flag).map(|f| f.map(run)) {
        Ok(None) => {
            print!("{help}");
            0
        }
        Ok(Some(Ok(code))) => code,
        Ok(Some(Err(e))) => {
            eprintln!("zazzle git {name}: {e}");
            1
        }
        Err(e) => {
            eprintln!("zazzle git {name}: {e}\n\n{help}");
            2
        }
    }
}

// Runs `zazzle git amend ARGS` and returns the exit code.
pub fn amend_main(args: &[String]) -> i32 {
    command("amend", AMEND_HELP, "--no-edit", args, amend)
}

// Runs `zazzle git undo ARGS` and returns the exit code.
pub fn undo_main(args: &[String]) -> i32 {
    command("undo", UNDO_HELP, "--force", args, |force| {
        undo(force).map(|done| {
            print!("{done}");
            0
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Turns words into arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks the one flag, help, and that nothing else is passed through.
    #[test]
    fn test_parse_flag() {
        assert_eq!(parse_flag(&v(&[]), "--force"), Ok(Some(false)));
        assert_eq!(parse_flag(&v(&["--force"]), "--force"), Ok(Some(true)));
        assert_eq!(parse_flag(&v(&["--force", "-h"]), "--force"), Ok(None));
        assert_eq!(parse_flag(&v(&["--help"]), "--no-edit"), Ok(None));
        assert!(parse_flag(&v(&["--signoff"]), "--no-edit").is_err_and(|e| e.contains("--signoff")));
    }

    // Checks the amend draft with and without staged changes.
    #[test]
    fn test_amend_draft() {
        let d = amend_draft("Fix it\n\nBody\n\n", '#', "M\ta.txt\nA\tb.txt\n");
        assert!(d.starts_with("Fix it\n\nBody\n\n# Amending"));
        assert!(d.ends_with("# Staged changes that will be added to it:\n#   M a.txt\n#   A b.txt\n"));
        assert!(amend_draft("x", ';', "").ends_with("; Nothing is staged: only the message changes.\n"));
    }

    // Checks the help exits and the usage error for an argument that is not passed through.
    #[test]
    fn test_main_help_and_usage() {
        assert_eq!(amend_main(&v(&["-h"])), 0);
        assert_eq!(undo_main(&v(&["--help"])), 0);
        assert_eq!(amend_main(&v(&["--amend"])), 2);
        assert_eq!(undo_main(&v(&["HEAD~2"])), 2);
    }
}
