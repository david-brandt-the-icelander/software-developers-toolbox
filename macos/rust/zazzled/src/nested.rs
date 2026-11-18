// Adds the instructions a client loads when its own tools touch a path (nested AGENTS.md and CLAUDE.md, path-scoped rules), which reads and edits through zazzle would otherwise skip.
use ignore::overrides::OverrideBuilder;
use std::path::{Path, PathBuf};

const NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
const MAX_BYTES: usize = 4000;

// Returns the instruction files that apply to the paths and that this session has not been sent, formatted to follow an answer.
pub fn for_paths(cwd: &Path, paths: &[PathBuf], first: &dyn Fn(&str) -> bool) -> String {
    let mut files: Vec<PathBuf> = Vec::new();
    for p in paths {
        for f in nested_files(cwd, p).into_iter().chain(scoped_rules(p)) {
            if !files.contains(&f) {
                files.push(f);
            }
        }
    }
    let mut out = String::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        if text.trim().is_empty() || !first(&format!("nested:{}", f.display())) {
            continue;
        }
        let shown = f.strip_prefix(cwd).unwrap_or(&f).display().to_string();
        let mut body = text.trim().to_string();
        if body.len() > MAX_BYTES {
            let cut = (0..=MAX_BYTES).rev().find(|i| body.is_char_boundary(*i)).unwrap_or(0);
            body.truncate(cut);
            body.push_str(&format!("\n[cut; zazzle read {shown} for the rest]"));
        }
        out.push_str(&format!("\n[instructions for this path: {shown}]\n{body}\n"));
    }
    out
}

// Returns AGENTS.md and CLAUDE.md in the folders between a path and its work tree's root, leaving out the folders the client already loaded (the working directory and above).
fn nested_files(cwd: &Path, path: &Path) -> Vec<PathBuf> {
    let Some(root) = crate::indexer::work_tree(path) else { return Vec::new() };
    let start = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
    let mut out = Vec::new();
    for dir in start.ancestors() {
        if cwd.starts_with(dir) || !dir.starts_with(root) {
            break;
        }
        out.extend(NAMES.iter().map(|n| dir.join(n)).filter(|f| f.is_file()));
    }
    out.reverse();
    out
}

// Returns the rules whose globs match a path: `.claude/rules/*.md` with `paths:` and `.github/instructions/*.instructions.md` with `applyTo:`.
fn scoped_rules(path: &Path) -> Vec<PathBuf> {
    let Some(root) = crate::indexer::work_tree(path) else { return Vec::new() };
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut out = Vec::new();
    for (dir, key) in [(".claude/rules", "paths"), (".github/instructions", "applyTo")] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else { continue };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        found.sort();
        for f in found {
            let globs = std::fs::read_to_string(&f).map(|t| front_globs(&t, key)).unwrap_or_default();
            if matches(root, rel, &globs) {
                out.push(f);
            }
        }
    }
    out
}

// Reads the globs one frontmatter key lists: `key: a, b`, `key: ["a", "b"]` or a `- a` list below it.
fn front_globs(text: &str, key: &str) -> Vec<String> {
    let Some(front) = text.strip_prefix("---\n").and_then(|t| t.split("\n---").next()) else {
        return Vec::new();
    };
    let clean = |s: &str| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    let mut lines = front.lines();
    while let Some(l) = lines.next() {
        let Some(v) = l.strip_prefix(key).and_then(|r| r.trim_start().strip_prefix(':')) else { continue };
        let v = v.trim().trim_start_matches('[').trim_end_matches(']');
        if !v.is_empty() {
            return v.split(',').map(clean).filter(|g| !g.is_empty()).collect();
        }
        return lines.map_while(|l| l.trim_start().strip_prefix("- ")).map(clean).collect();
    }
    Vec::new()
}

// True when a path below the root matches any of the globs.
fn matches(root: &Path, rel: &Path, globs: &[String]) -> bool {
    let mut b = OverrideBuilder::new(root);
    for g in globs {
        if b.add(g).is_err() {
            return false;
        }
    }
    !globs.is_empty() && b.build().is_ok_and(|o| o.matched(rel, false).is_whitelist())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks nested files below the working directory and matching rules are added once, and folders the client loaded are not.
    #[test]
    fn test_for_paths() {
        let t = TempDir::new();
        let root = t.path().canonicalize().expect("root");
        std::fs::create_dir_all(root.join(".git")).expect("git");
        t.write("AGENTS.md", b"root rules");
        t.write("api/AGENTS.md", b"api rules");
        t.write("api/v1/CLAUDE.md", b"v1 rules");
        t.write("api/v1/x.rs", b"fn x() {}");
        t.write("web/y.ts", b"y");
        t.write(".claude/rules/rust.md", b"---\npaths:\n  - \"api/**/*.rs\"\n---\nrust rule");
        t.write(
            ".github/instructions/ts.instructions.md",
            b"---\napplyTo: \"**/*.ts, docs/**\"\n---\nts rule",
        );
        t.write(".github/instructions/none.instructions.md", b"no front matter");
        t.write("api/v1/big/AGENTS.md", "z".repeat(5000).as_bytes());
        t.write("api/v1/big/z.rs", b"z");
        let seen = std::cell::RefCell::new(Vec::<String>::new());
        let first = |k: &str| {
            let new = !seen.borrow().iter().any(|s| s == k);
            seen.borrow_mut().push(k.into());
            new
        };
        let out = for_paths(&root, &[root.join("api/v1/x.rs")], &first);
        assert!(!out.contains("root rules"), "{out}");
        let (a, v) = (out.find("api rules").expect("api"), out.find("v1 rules").expect("v1"));
        assert!(
            a < v
                && out.contains("[instructions for this path: api/v1/CLAUDE.md]")
                && out.contains("rust rule"),
            "{out}"
        );
        assert_eq!(for_paths(&root, &[root.join("api/v1/x.rs")], &first), "");
        let ts = for_paths(&root, &[root.join("web/y.ts")], &first);
        assert!(ts.contains("ts rule") && !ts.contains("no front matter"), "{ts}");
        let big = for_paths(&root, &[root.join("api/v1/big/z.rs")], &first);
        assert!(big.contains("[cut; zazzle read api/v1/big/AGENTS.md for the rest]"), "{big}");
        assert_eq!(for_paths(&root.join("api"), &[root.join("api/AGENTS.md")], &|_| true), "");
        assert_eq!(for_paths(&root, &[PathBuf::from("/no/such/file")], &|_| true), "");
        t.write("api/v1/empty/AGENTS.md", b"  \n");
        t.write("api/v1/empty/e.rs", b"e");
        let twice = [root.join("api/v1/empty/e.rs"), root.join("api/v1/empty/e.rs")];
        let out = for_paths(&root, &twice, &|_| true);
        assert!(out.matches("api rules").count() == 1 && !out.contains("empty/AGENTS.md"), "{out}");
        let dir = for_paths(&root, &[root.join("api/v1")], &|_| true);
        assert!(dir.contains("v1 rules"), "{dir}");
        let outside = for_paths(Path::new("/elsewhere"), &[root.join("web/y.ts")], &|_| true);
        assert!(outside.contains("root rules") && outside.contains("ts rule"), "{outside}");
    }

    // Checks a read in a session carries a nested folder's rules once, and a read without a session carries none.
    #[test]
    fn test_read_adds_nested_rules() {
        let t = TempDir::new();
        let root = t.path().canonicalize().expect("root");
        std::fs::create_dir_all(root.join(".git")).expect("git");
        t.write("sub/AGENTS.md", b"sub rules");
        t.write("sub/a.txt", b"hello\nworld\n");
        let e = crate::engine::Engine::new();
        let first = crate::testutil::run_cmd_with(&e, &root, "s1", &["read", "sub/a.txt:1-1"]);
        assert!(
            first.text.contains("[instructions for this path: sub/AGENTS.md]\nsub rules"),
            "{}",
            first.text
        );
        let again = crate::testutil::run_cmd_with(&e, &root, "s1", &["read", "sub/a.txt", "--source"]);
        assert!(!again.text.contains("sub rules"), "{}", again.text);
        assert!(!crate::testutil::run_cmd(&root, &["read", "sub/a.txt"]).text.contains("sub rules"));
    }

    // Checks the frontmatter forms the rules use.
    #[test]
    fn test_front_globs() {
        assert_eq!(front_globs("---\npaths: [\"a\", 'b']\n---\n", "paths"), vec!["a", "b"]);
        assert_eq!(front_globs("---\napplyTo: '**'\n---", "applyTo"), vec!["**"]);
        assert!(front_globs("---\nother: x\n---", "paths").is_empty());
        assert!(front_globs("text", "paths").is_empty());
        assert!(!matches(Path::new("/r"), Path::new("a"), &[]));
        assert!(!matches(Path::new("/r"), Path::new("a"), &["[".into()]));
    }
}
