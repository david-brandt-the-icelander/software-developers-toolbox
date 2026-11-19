// Builds the prompt an agent gets: the instructions from the nearest .zazzle/commit-message-prompt.md (or a built-in default) and the changes.

use crate::files::{Entry, Mark};
use crate::git::git;
use std::path::{Path, PathBuf};

// The prompt file, looked up in each folder from the current one up to the home folder.
pub const PROMPT_FILE: &str = ".zazzle/commit-message-prompt.md";
// The instructions used when no prompt file is found.
pub const DEFAULT_PROMPT: &str = "Write a git commit message for the changes below.
- Subject line: imperative mood, at most 50 characters (never more than 72), no trailing period.
- If the change needs explaining, add a blank line and a short body wrapped at 72 columns that says what changed and why.
- Follow the style of the recent commit subjects when they show a convention.
- Reply with the commit message only: no code fences, no quotes, no commentary.";
// The most characters of changes put in a prompt.
const MAX_CHANGES: usize = 60_000;
// The most lines of a new file put in a prompt.
const NEW_FILE_LINES: usize = 200;

// Returns the nearest prompt file from the start folder upwards, stopping at home, then home's own if start is outside it.
pub fn find_prompt(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let p = dir.join(PROMPT_FILE);
        if p.is_file() {
            return Some(p);
        }
        if Some(dir) == home {
            return None;
        }
    }
    home.map(|h| h.join(PROMPT_FILE)).filter(|p| p.is_file())
}

// Returns the instructions: the prompt file's text if one is found and readable, else the default.
pub fn instructions(start: &Path, home: Option<&Path>) -> String {
    find_prompt(start, home)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PROMPT.to_string())
}

// Cuts text to the character limit, saying so when it does.
fn cap(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}\n[... cut at {max} characters]", &text[..at]),
        None => text.to_string(),
    }
}

// Joins the instructions, the branch, the recent subjects and the changes into one prompt.
pub fn assemble(instructions: &str, branch: &str, recent: &str, changes: &str) -> String {
    format!(
        "{}\n\nBranch: {branch}\n\nRecent commit subjects:\n{}\n\nChanges:\n{}\n",
        instructions.trim(),
        recent.trim(),
        cap(changes.trim(), MAX_CHANGES)
    )
}

// Returns the first lines of a new file, or a note when it is not text.
fn new_file(path: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(t) => {
            let lines: Vec<&str> = t.lines().take(NEW_FILE_LINES).collect();
            format!("--- new file {path}\n{}\n", lines.join("\n"))
        }
        Err(_) => format!("--- new file {path} (not text)\n"),
    }
}

// Describes what committing the marked files would change: a summary line per marked file, the diff of tracked files and the start of new ones.
pub fn marked_changes(entries: &[Entry], has_head: bool) -> String {
    let mut out = String::new();
    let mut tracked = Vec::new();
    for e in entries.iter().filter(|e| e.mark != Mark::Skip) {
        let verb = match e.mark {
            Mark::Add => "add",
            Mark::Ignore => "stop tracking and ignore",
            Mark::Remove => "stop tracking",
            _ => "delete",
        };
        out.push_str(&format!("{verb}: {}\n", e.path));
        if e.mark == Mark::Add && e.tracked() && has_head {
            tracked.push(e.path.as_str());
        }
    }
    if out.is_empty() {
        return "No files are marked for this commit.".to_string();
    }
    if !tracked.is_empty() {
        let mut args = vec!["diff", "HEAD", "--"];
        args.extend(tracked.iter());
        out.push('\n');
        out.push_str(&git(&args).unwrap_or_default());
    }
    for e in entries.iter().filter(|e| e.mark == Mark::Add && !(e.tracked() && has_head)) {
        out.push_str(&new_file(&e.path));
    }
    out
}

// Describes the staged changes, for when git itself opened the editor.
pub fn staged_changes() -> String {
    let diff = git(&["diff", "--cached"]).unwrap_or_default();
    if diff.trim().is_empty() {
        return "Nothing is staged.".to_string();
    }
    diff
}

// Builds the whole prompt for the current repository: instructions from start, then branch, recent subjects and the given changes.
pub fn prompt(start: &Path, home: Option<&Path>, changes: &str) -> String {
    let branch = git(&["branch", "--show-current"]).unwrap_or_default();
    let recent = git(&["log", "-10", "--format=%s"]).unwrap_or_default();
    assemble(&instructions(start, home), branch.trim(), &recent, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Makes an empty scratch directory.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zg-ctx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // Writes a prompt file in a folder.
    fn put(dir: &Path, text: &str) {
        std::fs::create_dir_all(dir.join(".zazzle")).unwrap();
        std::fs::write(dir.join(PROMPT_FILE), text).unwrap();
    }

    // Checks the cascade: the nearest file wins, the search stops at home, and home's file serves folders outside it.
    #[test]
    fn test_find_prompt_cascade() {
        let root = scratch("cascade");
        let home = root.join("home");
        let deep = home.join("repo/sub/dir");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_prompt(&deep, Some(&home)), None);
        put(&root, "above home");
        assert_eq!(find_prompt(&deep, Some(&home)), None);
        put(&home, "home");
        assert_eq!(find_prompt(&deep, Some(&home)), Some(home.join(PROMPT_FILE)));
        put(&home.join("repo"), "repo");
        assert_eq!(find_prompt(&deep, Some(&home)), Some(home.join("repo").join(PROMPT_FILE)));
        let outside = root.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(find_prompt(&outside, Some(&home)), Some(root.join(PROMPT_FILE)));
        std::fs::remove_file(root.join(PROMPT_FILE)).unwrap();
        assert_eq!(find_prompt(&outside, Some(&home)), Some(home.join(PROMPT_FILE)));
        assert_eq!(find_prompt(&outside, None), None);
        assert_eq!(instructions(&deep, Some(&home)), "repo");
        put(&home.join("repo"), "  ");
        assert_eq!(instructions(&deep, Some(&home)), DEFAULT_PROMPT);
        std::fs::remove_dir_all(root).unwrap();
    }

    // Checks how the prompt is put together and that long changes are cut.
    #[test]
    fn test_assemble_and_cap() {
        let p = assemble("Do it.\n", "main", "a\nb\n", "diff");
        assert_eq!(p, "Do it.\n\nBranch: main\n\nRecent commit subjects:\na\nb\n\nChanges:\ndiff\n");
        assert_eq!(cap("abcdef", 3), "abc\n[... cut at 3 characters]");
        assert_eq!(cap("abc", 3), "abc");
    }

    // Checks the description of marked files, new files and binary files, and the staged diff, in a scratch repository.
    #[test]
    fn test_marked_and_staged_changes() {
        let d = scratch("repo");
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "{args:?}");
        };
        run(&["init", "-q"]);
        let e = |path: &str, x: char, mark: Mark| Entry {
            path: d.join(path).to_string_lossy().into(),
            orig: None,
            x,
            y: ' ',
            mark,
        };
        std::fs::write(d.join("new.txt"), "hello\n").unwrap();
        std::fs::write(d.join("bin"), [0xff, 0xfe]).unwrap();
        let entries = vec![
            e("new.txt", '?', Mark::Add),
            e("bin", '?', Mark::Add),
            e("old", ' ', Mark::Delete),
            e("skip", '?', Mark::Skip),
        ];
        let text = marked_changes(&entries, false);
        assert!(text.contains("add: ") && text.contains("delete: ") && !text.contains("skip"));
        assert!(text.contains("hello") && text.contains("(not text)"));
        assert!(marked_changes(&entries[3..], false).contains("No files"));
        let marks = [Mark::Ignore, Mark::Remove].map(|m| e("t", ' ', m));
        assert!(marked_changes(&marks, false).contains("stop tracking and ignore"));
        assert!(git(&["--version"]).is_ok());
        assert!(!staged_changes().is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }
}
