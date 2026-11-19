// Recognises what kind of file is being edited from its name, and the rules and labels that go with each kind.

use std::path::Path;

// The kinds of file the editor knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Commit,
    Rebase,
    PullRequest,
    Changelog,
    Text,
}

// Git's scissors line, below which everything is ignored, without its leading comment character.
const SCISSORS: &str = " ------------------------ >8 ------------------------";

impl Kind {
    // Picks the kind from the file name.
    pub fn detect(path: &Path) -> Kind {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        match name {
            "COMMIT_EDITMSG" | "MERGE_MSG" | "TAG_EDITMSG" | "SQUASH_MSG" | "ZAZZLE_COMMIT_MSG" => {
                Kind::Commit
            }
            "git-rebase-todo" => Kind::Rebase,
            n if n.starts_with("CHANGELOG") => Kind::Changelog,
            n if n.ends_with(".md") => Kind::PullRequest,
            _ => Kind::Text,
        }
    }

    // Returns true for the files git writes, which carry a comment block.
    pub fn is_git(self) -> bool {
        matches!(self, Kind::Commit | Kind::Rebase)
    }

    // Returns the soft and hard length limits for a line, or None when lines are free.
    pub fn limits(self, row: usize) -> Option<(usize, usize)> {
        match (self, row) {
            (Kind::Commit, 0) => Some((50, 72)),
            (Kind::Commit, _) => Some((72, 72)),
            (Kind::Changelog, _) => Some((80, 80)),
            _ => None,
        }
    }

    // Names what Ctrl+S does for this kind.
    pub fn save_label(self) -> &'static str {
        match self {
            Kind::Commit => "Commit",
            Kind::Rebase => "Start rebase",
            _ => "Save",
        }
    }

    // Names what Esc does for this kind.
    pub fn cancel_label(self) -> &'static str {
        match self {
            Kind::Rebase => "Abort rebase",
            _ => "Cancel",
        }
    }
}

// Splits file text into editable lines and the read-only tail: git's trailing comment block and everything from the scissors line on.
pub fn split(text: &str, kind: Kind, comment: char) -> (Vec<String>, Vec<String>) {
    let mut lines: Vec<String> = text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    if !kind.is_git() {
        return (lines, Vec::new());
    }
    let is_comment = |l: &str| l.starts_with(comment);
    let scissors = format!("{comment}{SCISSORS}");
    let mut start = lines.iter().position(|l| *l == scissors).unwrap_or(lines.len());
    while start > 0 && (is_comment(&lines[start - 1]) || lines[start - 1].is_empty()) {
        start -= 1;
    }
    while start < lines.len() && lines[start].is_empty() {
        start += 1;
    }
    let tail = lines.split_off(start);
    (lines, tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks kind detection from file names.
    #[test]
    fn test_kind_detect() {
        assert_eq!(Kind::detect(Path::new("/r/.git/COMMIT_EDITMSG")), Kind::Commit);
        assert_eq!(Kind::detect(Path::new("ZAZZLE_COMMIT_MSG")), Kind::Commit);
        assert_eq!(Kind::detect(Path::new(".git/rebase-merge/git-rebase-todo")), Kind::Rebase);
        assert_eq!(Kind::detect(Path::new("CHANGELOG.md")), Kind::Changelog);
        assert_eq!(Kind::detect(Path::new("/tmp/123.md")), Kind::PullRequest);
        assert_eq!(Kind::detect(Path::new("notes.txt")), Kind::Text);
        assert_eq!(Kind::detect(Path::new("/")), Kind::Text);
    }

    // Checks limits and labels per kind.
    #[test]
    fn test_kind_rules_and_labels() {
        assert_eq!(Kind::Commit.limits(0), Some((50, 72)));
        assert_eq!(Kind::Commit.limits(2), Some((72, 72)));
        assert_eq!(Kind::Changelog.limits(0), Some((80, 80)));
        assert_eq!(Kind::Text.limits(0), None);
        assert_eq!(Kind::Commit.save_label(), "Commit");
        assert_eq!(Kind::Rebase.save_label(), "Start rebase");
        assert_eq!(Kind::Text.save_label(), "Save");
        assert_eq!(Kind::Rebase.cancel_label(), "Abort rebase");
        assert_eq!(Kind::Commit.cancel_label(), "Cancel");
        assert!(!Kind::PullRequest.is_git());
    }

    // Checks the split of git's comment block, the blank line before it and the scissors line.
    #[test]
    fn test_split_comment_tail() {
        let (e, t) = split("subject\n\nbody\n\n# Please enter\n#\n", Kind::Commit, '#');
        assert_eq!(e, vec!["subject", "", "body", ""]);
        assert_eq!(t, vec!["# Please enter", "#"]);
        let (e, t) = split("\n# c\n", Kind::Commit, '#');
        assert_eq!(e, vec![""]);
        assert_eq!(t, vec!["# c"]);
        let sc = "msg\n; x\n; ------------------------ >8 ------------------------\ndiff\n# not a comment\n";
        let (e, t) = split(sc, Kind::Commit, ';');
        assert_eq!(e, vec!["msg"]);
        assert_eq!(t.len(), 4);
        let (e, t) = split("a\r\n# b", Kind::Text, '#');
        assert_eq!((e, t.len()), (vec!["a".to_string(), "# b".to_string()], 0));
        let (e, t) = split("", Kind::Commit, '#');
        assert_eq!((e.len(), t.len()), (1, 0));
    }
}
