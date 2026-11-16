// The Files view's model: changed files from git status, the mark on each, saving marks for the next run, and the git actions they become.

use serde_json::{json, Value};

// What will happen to a file when committing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Skip,
    Add,
    Ignore,
    Remove,
    Delete,
}

// One changed file: its path, the path it was renamed from, git's two status letters and its mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub orig: Option<String>,
    pub x: char,
    pub y: char,
    pub mark: Mark,
}

// One git step that carries out a mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    DeleteFile(String),
    AppendIgnore(String),
    RmCached(String),
    Add(Vec<String>),
    Unstage(Vec<String>),
}

impl Mark {
    // Returns the label shown in the Files view, padded so file names line up.
    pub fn label(self) -> &'static str {
        match self {
            Mark::Skip => "[      ]",
            Mark::Add => "[ADD]   ",
            Mark::Ignore => "[IGNORE]",
            Mark::Remove => "[REMOVE]",
            Mark::Delete => "[DELETE]",
        }
    }

    // Returns the name stored in the saved marks file.
    fn name(self) -> &'static str {
        match self {
            Mark::Skip => "skip",
            Mark::Add => "add",
            Mark::Ignore => "ignore",
            Mark::Remove => "remove",
            Mark::Delete => "delete",
        }
    }

    // Reads a stored mark name.
    fn from_name(s: &str) -> Option<Mark> {
        [Mark::Skip, Mark::Add, Mark::Ignore, Mark::Remove, Mark::Delete].into_iter().find(|m| m.name() == s)
    }
}

impl Entry {
    // Returns true if git tracks the file (it is not a new, untracked file).
    pub fn tracked(&self) -> bool {
        self.x != '?'
    }

    // Returns true if some of the file's changes are already staged.
    pub fn staged(&self) -> bool {
        self.x != ' ' && self.x != '?'
    }

    // Returns the paths git commands need for this entry: the path and, for a rename, the old path.
    fn paths(&self) -> Vec<String> {
        let mut v = vec![self.path.clone()];
        v.extend(self.orig.clone());
        v
    }

    // Moves the mark to the next one in the Space cycle, skipping Remove for untracked files.
    pub fn cycle(&mut self) {
        self.mark = match self.mark {
            Mark::Skip => Mark::Add,
            Mark::Add => Mark::Ignore,
            Mark::Ignore if self.tracked() => Mark::Remove,
            _ => Mark::Skip,
        };
    }

    // Returns the row text shown in the Files view.
    pub fn row(&self) -> String {
        let name = match &self.orig {
            Some(o) => format!("{o} -> {}", self.path),
            None => self.path.clone(),
        };
        format!("{} {}{}  {name}", self.mark.label(), self.x, self.y)
    }
}

// Parses `git status --porcelain=v1 -z` output into entries with the starting marks.
pub fn parse_status(out: &str, no_add: bool) -> Vec<Entry> {
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    let mut entries = Vec::new();
    while let Some(f) = fields.next() {
        let mut chars = f.chars();
        let (Some(x), Some(y)) = (chars.next(), chars.next()) else { continue };
        let path = f.get(3..).unwrap_or("").to_string();
        let orig = if x == 'R' || x == 'C' { fields.next().map(str::to_string) } else { None };
        let mut e = Entry { path, orig, x, y, mark: Mark::Skip };
        e.mark = default_mark(&e, no_add);
        entries.push(e);
    }
    entries
}

// Returns the starting mark: like `git commit -a` (tracked changes added), or only what is staged with --no-add.
fn default_mark(e: &Entry, no_add: bool) -> Mark {
    let wanted = if no_add { e.staged() } else { e.tracked() };
    if wanted {
        Mark::Add
    } else {
        Mark::Skip
    }
}

// Sets every mark to Add, or every mark to Skip if all were already Add.
pub fn toggle_all(entries: &mut [Entry]) {
    let to = if entries.iter().all(|e| e.mark == Mark::Add) { Mark::Skip } else { Mark::Add };
    for e in entries {
        e.mark = to;
    }
}

// Serialises the marks with the commit they were made on.
pub fn save_marks(entries: &[Entry], head: &str) -> String {
    let marks: serde_json::Map<String, Value> =
        entries.iter().map(|e| (e.path.clone(), json!(e.mark.name()))).collect();
    json!({ "head": head, "marks": marks }).to_string()
}

// Applies saved marks to the entries if they were made on the same commit; returns true if they were applied.
pub fn load_marks(entries: &mut [Entry], saved: &str, head: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(saved) else { return false };
    if v["head"].as_str() != Some(head) {
        return false;
    }
    for e in entries.iter_mut() {
        if let Some(m) = v["marks"][&e.path].as_str().and_then(Mark::from_name) {
            e.mark = m;
        }
    }
    true
}

// Returns the files marked for deletion.
pub fn deletions(entries: &[Entry]) -> Vec<String> {
    entries.iter().filter(|e| e.mark == Mark::Delete).map(|e| e.path.clone()).collect()
}

// Turns marks into git steps, in the order delete, ignore, remove, add, unstage; a staged file keeps its staged part under --no-add.
pub fn plan(entries: &[Entry], no_add: bool) -> Vec<Action> {
    let mut deletes = Vec::new();
    let mut ignores = Vec::new();
    let mut removes = Vec::new();
    let mut adds = Vec::new();
    let mut unstage = Vec::new();
    for e in entries {
        match e.mark {
            Mark::Delete => {
                deletes.push(Action::DeleteFile(e.path.clone()));
                if e.tracked() {
                    adds.extend(e.paths());
                }
            }
            Mark::Ignore => {
                ignores.push(Action::AppendIgnore(e.path.clone()));
                if e.tracked() {
                    removes.push(Action::RmCached(e.path.clone()));
                }
            }
            Mark::Remove => removes.push(Action::RmCached(e.path.clone())),
            Mark::Add if no_add && e.staged() => {}
            Mark::Add => adds.extend(e.paths()),
            Mark::Skip if e.staged() => unstage.extend(e.paths()),
            Mark::Skip => {}
        }
    }
    if !ignores.is_empty() {
        adds.push(".gitignore".to_string());
    }
    let mut out = deletes;
    out.extend(ignores);
    out.extend(removes);
    if !adds.is_empty() {
        out.push(Action::Add(adds));
    }
    if !unstage.is_empty() {
        out.push(Action::Unstage(unstage));
    }
    out
}

// Returns the .gitignore text with an entry for the path added, unless it is already listed.
pub fn add_ignore(gitignore: &str, path: &str) -> Option<String> {
    let line = format!("/{path}");
    if gitignore.lines().any(|l| l.trim() == line || l.trim() == path) {
        return None;
    }
    let mut out = gitignore.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&line);
    out.push('\n');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Builds an entry with a mark.
    fn e(path: &str, x: char, y: char, mark: Mark) -> Entry {
        Entry { path: path.into(), orig: None, x, y, mark }
    }

    // Checks parsing porcelain output, renames and the starting marks with and without --no-add.
    #[test]
    fn test_parse_status_defaults() {
        let copied = parse_status("C  c.rs\0src.rs\0", false);
        assert_eq!(copied[0].orig.as_deref(), Some("src.rs"));
        let out = " M a.rs\0M  b.rs\0?? new.txt\0R  n.rs\0o.rs\0 D gone.rs\0x\0";
        let v = parse_status(out, false);
        assert_eq!(v.len(), 5);
        assert_eq!(v[0], e("a.rs", ' ', 'M', Mark::Add));
        assert_eq!(v[2].mark, Mark::Skip);
        assert_eq!(v[3].orig.as_deref(), Some("o.rs"));
        assert_eq!(v[4].mark, Mark::Add);
        let n = parse_status(out, true);
        let marks: Vec<Mark> = n.iter().map(|e| e.mark).collect();
        assert_eq!(marks, vec![Mark::Skip, Mark::Add, Mark::Skip, Mark::Add, Mark::Skip]);
        assert_eq!(v[3].row(), "[ADD]    R   o.rs -> n.rs");
        assert_eq!(v[2].row(), "[      ] ??  new.txt");
    }

    // Checks the Space cycle for tracked and untracked files, and labels.
    #[test]
    fn test_entry_cycle() {
        let mut t = e("a", ' ', 'M', Mark::Skip);
        let mut seen = Vec::new();
        for _ in 0..4 {
            t.cycle();
            seen.push(t.mark);
        }
        assert_eq!(seen, vec![Mark::Add, Mark::Ignore, Mark::Remove, Mark::Skip]);
        let mut u = e("b", '?', '?', Mark::Ignore);
        u.cycle();
        assert_eq!(u.mark, Mark::Skip);
        let mut d = e("c", ' ', 'M', Mark::Delete);
        d.cycle();
        assert_eq!(d.mark, Mark::Skip);
        for m in [Mark::Skip, Mark::Add, Mark::Ignore, Mark::Remove, Mark::Delete] {
            assert_eq!(m.label().chars().count(), 8);
        }
    }

    // Checks toggling every mark to Add and back.
    #[test]
    fn test_toggle_all() {
        let mut v = vec![e("a", ' ', 'M', Mark::Add), e("b", '?', '?', Mark::Delete)];
        toggle_all(&mut v);
        assert!(v.iter().all(|e| e.mark == Mark::Add));
        toggle_all(&mut v);
        assert!(v.iter().all(|e| e.mark == Mark::Skip));
    }

    // Checks saving and loading marks, and that marks from another commit are ignored.
    #[test]
    fn test_marks_persist() {
        let v = vec![e("a", ' ', 'M', Mark::Ignore), e("b", '?', '?', Mark::Delete)];
        let saved = save_marks(&v, "abc");
        let mut fresh = vec![e("a", ' ', 'M', Mark::Add), e("c", ' ', 'M', Mark::Add)];
        assert!(load_marks(&mut fresh, &saved, "abc"));
        assert_eq!(fresh[0].mark, Mark::Ignore);
        assert_eq!(fresh[1].mark, Mark::Add);
        let mut other = vec![e("a", ' ', 'M', Mark::Add)];
        assert!(!load_marks(&mut other, &saved, "def"));
        assert!(!load_marks(&mut other, "not json", "abc"));
        assert_eq!(other[0].mark, Mark::Add);
        assert_eq!(deletions(&v), vec!["b".to_string()]);
    }

    // Checks the git steps each mark becomes and their order.
    #[test]
    fn test_plan() {
        let mut ren = e("n", 'R', ' ', Mark::Add);
        ren.orig = Some("o".into());
        let v = vec![
            e("del-tracked", ' ', 'M', Mark::Delete),
            e("del-new", '?', '?', Mark::Delete),
            e("ign-tracked", ' ', 'M', Mark::Ignore),
            e("ign-new", '?', '?', Mark::Ignore),
            e("rm", ' ', 'M', Mark::Remove),
            ren,
            e("staged-skip", 'M', ' ', Mark::Skip),
            e("plain-skip", ' ', 'M', Mark::Skip),
            e("staged-add", 'M', 'M', Mark::Add),
        ];
        let p = plan(&v, false);
        assert_eq!(p[0], Action::DeleteFile("del-tracked".into()));
        assert_eq!(p[1], Action::DeleteFile("del-new".into()));
        assert_eq!(p[2], Action::AppendIgnore("ign-tracked".into()));
        assert_eq!(p[3], Action::AppendIgnore("ign-new".into()));
        assert_eq!(p[4], Action::RmCached("ign-tracked".into()));
        assert_eq!(p[5], Action::RmCached("rm".into()));
        let adds: Vec<String> =
            ["del-tracked", "n", "o", "staged-add", ".gitignore"].iter().map(|s| s.to_string()).collect();
        assert_eq!(p[6], Action::Add(adds));
        assert_eq!(p[7], Action::Unstage(vec!["staged-skip".into()]));
        let q = plan(&v[8..], true);
        assert!(q.is_empty());
        assert!(plan(&[], false).is_empty());
    }

    // Checks adding a path to .gitignore once.
    #[test]
    fn test_add_ignore() {
        assert_eq!(add_ignore("", "a.log").as_deref(), Some("/a.log\n"));
        assert_eq!(add_ignore("x", "a.log").as_deref(), Some("x\n/a.log\n"));
        assert_eq!(add_ignore("x\n", "a.log").as_deref(), Some("x\n/a.log\n"));
        assert_eq!(add_ignore("/a.log\n", "a.log"), None);
        assert_eq!(add_ignore("a.log\n", "a.log"), None);
    }
}
