// `zazzle git commit`: keeps a draft message and file marks for the next commit, opens the editor, stages by the marks and runs git commit.

use crate::editor::Outcome;
use crate::files::{self, Action, Entry};
use crate::git::{git, git_interactive};
use crate::message;
use crate::offer;
use std::fs;
use std::path::{Path, PathBuf};

// The help text for `zazzle git commit`.
pub const HELP: &str = "usage: zazzle git commit [--no-add] [GIT COMMIT OPTIONS]

Opens the commit message editor on a draft kept in .git (so a failed hook or a
cancel never loses the message), then commits. The message is on the left and
the changed files on the right; Tab moves between them. Each file is marked
[ADD], [IGNORE], [REMOVE], [DELETE] or left out.

While you write, an agent CLI (claude, codex or gemini, whichever is installed)
composes a suggestion; Ctrl+S with an empty message commits it (waiting for it
if needed). Ctrl+N picks the next agent, Ctrl+E the next effort level; both are
remembered. The agent's instructions come from the nearest
.zazzle/commit-message-prompt.md, looking up from the current folder to your
home folder, or a built-in default.

By default changed tracked files start as [ADD], like `git commit -a`.
  --no-add   start with only what is already staged marked [ADD]
With -m, -F, -C or --no-edit the editor is skipped and git commit runs directly
(with -a unless --no-add). Other options (--amend, --signoff, ...) go to git commit.

The first run asks once whether to make `zazzle git message` git's editor, so
plain `git commit`, `git rebase -i` and `git merge` use it too.
";

// The draft message file inside the git directory.
const DRAFT: &str = "ZAZZLE_COMMIT_MSG";
// The saved file marks inside the git directory.
const MARKS: &str = "ZAZZLE_COMMIT_FILES";

// The parsed command line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub no_add: bool,
    pub help: bool,
    pub amend: bool,
    pub has_message: bool,
    pub git_args: Vec<String>,
}

// Splits our options from the ones passed on to git commit.
pub fn parse_args(args: &[String]) -> Args {
    let mut a = Args::default();
    for arg in args {
        match arg.as_str() {
            "--no-add" => a.no_add = true,
            "-h" | "--help" => a.help = true,
            s => {
                let msg = ["-m", "--message", "-F", "--file", "-C", "--reuse-message", "--no-edit"];
                if msg.iter().any(|m| s == *m || s.starts_with(&format!("{m}=")))
                    || (s.starts_with("-m") && s.len() > 2)
                {
                    a.has_message = true;
                }
                a.amend |= s == "--amend";
                a.git_args.push(arg.clone());
            }
        }
    }
    a
}

// Returns the starting draft: the amended commit's message or commit.template, then guidance as comments.
pub fn initial_draft(start: &str, comment: char, branch: &str) -> String {
    let c = comment;
    let mut s = start.trim_end().to_string();
    s.push_str("\n\n");
    s.push_str(&format!("{c} Write the commit message above; lines starting with '{c}' are ignored.\n"));
    s.push_str(&format!("{c} Tab: choose the files for this commit. Ctrl+S commits, Esc cancels.\n"));
    if !branch.is_empty() {
        s.push_str(&format!("{c} On branch {branch}\n"));
    }
    s
}

// Returns the text the draft starts from: the last message when amending, else the commit template, else nothing.
fn draft_start(amend: bool) -> String {
    if amend {
        return git(&["log", "-1", "--format=%B"]).unwrap_or_default();
    }
    let template = git(&["config", "--path", "--get", "commit.template"]).unwrap_or_default();
    match template.trim() {
        "" => String::new(),
        t => fs::read_to_string(t).unwrap_or_default(),
    }
}

// Carries out the git steps for the marks, from the repository root.
pub fn apply(actions: &[Action], has_head: bool) -> Result<(), String> {
    for a in actions {
        match a {
            Action::DeleteFile(p) => fs::remove_file(p).map_err(|e| format!("delete {p}: {e}"))?,
            Action::AppendIgnore(p) => {
                let old = fs::read_to_string(".gitignore").unwrap_or_default();
                if let Some(new) = files::add_ignore(&old, p) {
                    fs::write(".gitignore", new).map_err(|e| format!(".gitignore: {e}"))?;
                }
            }
            Action::RmCached(p) => {
                git(&["rm", "--cached", "-q", "--", p])?;
            }
            Action::Add(paths) => run_paths(&["add", "-A", "--"], paths)?,
            Action::Unstage(paths) if has_head => run_paths(&["reset", "-q", "--"], paths)?,
            Action::Unstage(paths) => run_paths(&["rm", "--cached", "-q", "--"], paths)?,
        }
    }
    Ok(())
}

// Runs git with fixed arguments followed by paths.
fn run_paths(fixed: &[&str], paths: &[String]) -> Result<(), String> {
    let mut args: Vec<&str> = fixed.to_vec();
    args.extend(paths.iter().map(String::as_str));
    git(&args).map(|_| ())
}

// Returns a path inside the git directory.
fn git_path(name: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(git(&["rev-parse", "--git-path", name])?.trim()))
}

// Runs git commit with the message file, removing the draft and marks when it succeeds.
fn commit(draft: &Path, marks: &Path, git_args: &[String]) -> i32 {
    let mut args: Vec<String> = ["commit", "--cleanup=strip", "-F"].iter().map(|s| s.to_string()).collect();
    args.push(draft.to_string_lossy().into_owned());
    args.extend(git_args.iter().cloned());
    let code = git_interactive(&args);
    if code == 0 {
        let _ = fs::remove_file(draft);
        let _ = fs::remove_file(marks);
    }
    code
}

// Loads the changed files with their marks: the starting marks, then any saved marks made on the same commit.
fn load_entries(marks: &Path, head: &str, no_add: bool) -> Result<Vec<Entry>, String> {
    let status = git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let mut entries = files::parse_status(&status, no_add);
    if let Ok(saved) = fs::read_to_string(marks) {
        files::load_marks(&mut entries, &saved, head);
    }
    Ok(entries)
}

// Runs the whole commit flow and returns git's exit code, or 1 when cancelled or failed.
fn run(a: &Args) -> Result<i32, String> {
    if a.has_message {
        let mut args = vec!["commit".to_string()];
        if !a.no_add {
            args.push("-a".to_string());
        }
        args.extend(a.git_args.iter().cloned());
        return Ok(git_interactive(&args));
    }
    let origin = std::env::current_dir().map_err(|e| e.to_string())?;
    let top = git(&["rev-parse", "--show-toplevel"])?;
    std::env::set_current_dir(top.trim()).map_err(|e| e.to_string())?;
    let draft = git_path(DRAFT)?;
    let marks = git_path(MARKS)?;
    let head = git(&["rev-parse", "-q", "--verify", "HEAD"]).unwrap_or_default().trim().to_string();
    let entries = load_entries(&marks, &head, a.no_add)?;
    if entries.is_empty() && !a.amend {
        return Err("nothing to commit".to_string());
    }
    offer::offer();
    if !draft.exists() {
        let branch = git(&["branch", "--show-current"]).unwrap_or_default();
        let text = initial_draft(&draft_start(a.amend), message::comment_char(), branch.trim());
        fs::write(&draft, text).map_err(|e| format!("{}: {e}", draft.display()))?;
    }
    let edited = message::edit(&draft, entries, &origin)?;
    if edited.outcome == Outcome::Cancel {
        return Ok(1);
    }
    fs::write(&marks, files::save_marks(&edited.files, &head)).map_err(|e| e.to_string())?;
    apply(&files::plan(&edited.files, a.no_add), !head.is_empty())?;
    Ok(commit(&draft, &marks, &a.git_args))
}

// Runs `zazzle git commit ARGS` and returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let a = parse_args(args);
    if a.help {
        print!("{HELP}\n{}", offer::HOW_TO);
        return 0;
    }
    match run(&a) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("zazzle git commit: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Turns words into arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks option parsing and what is passed on to git.
    #[test]
    fn test_parse_args() {
        let a = parse_args(&v(&["--no-add", "--amend", "--signoff"]));
        assert!(a.no_add && a.amend && !a.has_message && !a.help);
        assert_eq!(a.git_args, v(&["--amend", "--signoff"]));
        assert!(parse_args(&v(&["-m", "x"])).has_message);
        assert!(parse_args(&v(&["-mfix"])).has_message);
        assert!(parse_args(&v(&["--message=x"])).has_message);
        assert!(parse_args(&v(&["--no-edit"])).has_message);
        assert!(parse_args(&v(&["--help"])).help);
        assert!(!parse_args(&v(&["--signoff"])).has_message);
    }

    // Checks the starting draft with and without a branch.
    #[test]
    fn test_initial_draft() {
        let d = initial_draft("Template\n\n", ';', "main");
        assert!(d.starts_with("Template\n\n; Write"));
        assert!(d.ends_with("; On branch main\n"));
        assert!(!initial_draft("", '#', "").contains("On branch"));
    }

    // Checks the help exit.
    #[test]
    fn test_main_help() {
        assert_eq!(main(&v(&["-h"])), 0);
    }
}
