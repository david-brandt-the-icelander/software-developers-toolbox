// The one-time offer, on the first `zazzle git commit`, to make `zazzle git message` git's editor for every repository.

use crate::git::git;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};

// The core.editor value the offer sets.
pub const EDITOR: &str = "zazzle git message";
// The global git config key that records that the offer was made, so it is made only once.
pub const OFFERED_KEY: &str = "zazzle.editorOffered";
// How to make zazzle git message git's editor by hand, shown in the help texts.
pub const HOW_TO: &str = "Make `zazzle git message` git's editor for every repository
(`zazzle git commit` offers this once):

  git config --global core.editor 'zazzle git message'

Undo: git config --global --unset core.editor
";

// Returns true if a core.editor value already runs the zazzle message editor.
pub fn is_zazzle_editor(value: &str) -> bool {
    ["zazzle git message", "z git message", "zazzle-git-commit-message-editor"]
        .iter()
        .any(|n| value.contains(n))
}

// Returns the question to ask, or None when zazzle is already git's editor or the offer was made before.
pub fn question(editor: &str, offered: bool) -> Option<String> {
    if offered || is_zazzle_editor(editor) {
        return None;
    }
    let current = if editor.is_empty() { String::new() } else { format!(" (now: {editor})") };
    Some(format!(
        "zazzle git: make `zazzle git message` git's editor for every repository{current}?\n\
         This runs: git config --global core.editor '{EDITOR}'   (asked only once)\n[y/N] "
    ))
}

// Returns true for an answer that means yes.
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

// Sets the editor if the answer is yes, records that the offer was made, and returns what to tell the user.
pub fn apply(answer: &str) -> String {
    let _ = git(&["config", "--global", OFFERED_KEY, "true"]);
    if !is_yes(answer) {
        return "zazzle git: editor not changed; `zazzle git help commit` shows how to set it later.\n"
            .to_string();
    }
    match git(&["config", "--global", "core.editor", EDITOR]) {
        Ok(_) => "zazzle git: git's editor is now `zazzle git message`. Undo: git config --global --unset core.editor\n".to_string(),
        Err(e) => format!("zazzle git: could not set core.editor: {e}\n"),
    }
}

// Asks on the terminal, once, whether to make zazzle git message git's editor; does nothing without a terminal.
pub fn offer() {
    let editor = git(&["config", "--get", "core.editor"]).unwrap_or_default();
    let offered = git(&["config", "--global", "--get", OFFERED_KEY]).is_ok_and(|v| v.trim() == "true");
    let Some(q) = question(editor.trim(), offered) else { return };
    let Ok(mut tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") else { return };
    let _ = tty.write_all(q.as_bytes());
    let mut answer = String::new();
    let _ = BufReader::new(&tty).read_line(&mut answer);
    let _ = tty.write_all(apply(&answer).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks which editor values count as zazzle already.
    #[test]
    fn test_is_zazzle_editor() {
        assert!(is_zazzle_editor("zazzle git message"));
        assert!(is_zazzle_editor("z git message"));
        assert!(is_zazzle_editor("/opt/bin/zazzle-git-commit-message-editor"));
        assert!(!is_zazzle_editor("vim"));
        assert!(!is_zazzle_editor(""));
    }

    // Checks when the question is asked and that it names the current editor.
    #[test]
    fn test_question() {
        assert!(question("", false).is_some_and(|q| q.contains("[y/N]") && !q.contains("now:")));
        assert!(question("vim", false).is_some_and(|q| q.contains("(now: vim)")));
        assert_eq!(question("vim", true), None);
        assert_eq!(question("zazzle git message", false), None);
    }

    // Checks the answers that mean yes.
    #[test]
    fn test_is_yes() {
        assert!(is_yes("y\n"));
        assert!(is_yes(" YES "));
        assert!(!is_yes("\n"));
        assert!(!is_yes("n"));
    }
}
