// Adds the instructions a client loads when its own tools touch a path (nested AGENTS.md and CLAUDE.md, path-scoped rules), which reads and edits through zazzle would otherwise skip.
use ignore::overrides::OverrideBuilder;
use std::path::{Path, PathBuf};
use zazzle_agent_rules_list::{Found, Kind};

const NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

// Returns the instruction files that apply to the paths, narrowed to the active purposes, minus the files and line ranges this recipient was already sent by any route (get-rules included), formatted to follow an answer.
pub fn for_paths(
    cwd: &Path,
    paths: &[PathBuf],
    purposes: &[String],
    ledger: &crate::rules::Ledger,
) -> String {
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
        let Some(found) = narrow(&f, text, purposes) else { continue };
        let Some(part) = crate::rules::fresh(&found, ledger) else { continue };
        out.push_str(&format_part(cwd, &part));
    }
    out
}

// Builds the rule file as found for the purposes: whole when none are active or it has no markers, the marked ranges otherwise, None when it is empty or has nothing for them.
fn narrow(path: &Path, text: String, purposes: &[String]) -> Option<Found> {
    use zazzle_agent_rules_list::purpose::{part, Part};
    if text.trim().is_empty() {
        return None;
    }
    let lines = match purposes.is_empty() {
        true => Vec::new(),
        false => match part(&text, purposes) {
            Part::Whole => Vec::new(),
            Part::Lines(l) => l,
            Part::Nothing => return None,
        },
    };
    Some(Found { kind: Kind::Rule, path: path.to_path_buf(), lines, text: Some(text) })
}

// Formats one delivered rule file or its ranges under an `[instructions for this path: FILE[:A-B]]` line, without purpose markers.
fn format_part(cwd: &Path, f: &Found) -> String {
    let shown = f.path.strip_prefix(cwd).unwrap_or(&f.path).display().to_string();
    let text = f.text.as_deref().unwrap_or_default();
    if f.lines.is_empty() {
        return format!("\n[instructions for this path: {shown}]\n{}\n", text.trim());
    }
    let all: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    for &(a, b) in &f.lines {
        let body: Vec<&str> = all
            .iter()
            .take(b)
            .skip(a.saturating_sub(1))
            .copied()
            .filter(|l| !zazzle_agent_rules_list::purpose::is_marker(l))
            .collect();
        out.push_str(&format!(
            "\n[instructions for this path: {shown}:{a}-{b}]\n{}\n",
            body.join("\n").trim()
        ));
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
        let (has, mark) =
            (|k: &str| seen.borrow().iter().any(|s| s == k), |k: &str| seen.borrow_mut().push(k.into()));
        let first = crate::rules::Ledger { has: &has, mark: &mark };
        let (never, skip) = (|_: &str| false, |_: &str| {});
        let fresh = crate::rules::Ledger { has: &never, mark: &skip };
        let out = for_paths(&root, &[root.join("api/v1/x.rs")], &[], &first);
        assert!(!out.contains("root rules"), "{out}");
        let (a, v) = (out.find("api rules").expect("api"), out.find("v1 rules").expect("v1"));
        assert!(
            a < v
                && out.contains("[instructions for this path: api/v1/CLAUDE.md]")
                && out.contains("rust rule"),
            "{out}"
        );
        assert_eq!(for_paths(&root, &[root.join("api/v1/x.rs")], &[], &first), "");
        let ts = for_paths(&root, &[root.join("web/y.ts")], &[], &first);
        assert!(ts.contains("ts rule") && !ts.contains("no front matter"), "{ts}");
        let big = for_paths(&root, &[root.join("api/v1/big/z.rs")], &[], &first);
        assert!(big.contains(&"z".repeat(5000)), "whole files, no cut");
        assert_eq!(for_paths(&root.join("api"), &[root.join("api/AGENTS.md")], &[], &fresh), "");
        assert_eq!(for_paths(&root, &[PathBuf::from("/no/such/file")], &[], &fresh), "");
        t.write("api/v1/empty/AGENTS.md", b"  \n");
        t.write("api/v1/empty/e.rs", b"e");
        let twice = [root.join("api/v1/empty/e.rs"), root.join("api/v1/empty/e.rs")];
        let out = for_paths(&root, &twice, &[], &fresh);
        assert!(out.matches("api rules").count() == 1 && !out.contains("empty/AGENTS.md"), "{out}");
        let dir = for_paths(&root, &[root.join("api/v1")], &[], &fresh);
        assert!(dir.contains("v1 rules"), "{dir}");
        let outside = for_paths(Path::new("/elsewhere"), &[root.join("web/y.ts")], &[], &fresh);
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
            first.notes.contains("[instructions for this path: sub/AGENTS.md]\nsub rules"),
            "{}",
            first.notes
        );
        assert!(!first.text.contains("sub rules"), "rules stay out of the source: {}", first.text);
        let again = crate::testutil::run_cmd_with(&e, &root, "s1", &["read", "sub/a.txt", "--source"]);
        assert!(!again.notes.contains("sub rules"), "{}", again.notes);
        assert!(crate::testutil::run_cmd(&root, &["read", "sub/a.txt"]).notes.is_empty());
    }

    // Checks the active purpose narrows the added instructions to its marked ranges, each range goes once, and a new purpose adds only its own.
    #[test]
    fn test_purposes_and_ranges() {
        let t = TempDir::new();
        let root = t.path().canonicalize().expect("root");
        std::fs::create_dir_all(root.join(".git")).expect("git");
        t.write(
            "sub/AGENTS.md",
            b"<!-- #zazzle-purpose-begin:build -->\nbuild rule\n<!-- #zazzle-purpose-end:build -->\n<!-- #zazzle-purpose-begin:review -->\nreview rule\n<!-- #zazzle-purpose-end:review -->\n",
        );
        t.write("sub/a.txt", b"x\n");
        t.write("other/AGENTS.md", b"<!-- #zazzle-purpose:deploy -->\nonly for deploys\n");
        t.write("other/b.txt", b"y\n");
        t.write("blank/AGENTS.md", b"  \n");
        let seen = std::cell::RefCell::new(Vec::<String>::new());
        let (has, mark) =
            (|k: &str| seen.borrow().iter().any(|s| s == k), |k: &str| seen.borrow_mut().push(k.into()));
        let l = crate::rules::Ledger { has: &has, mark: &mark };
        let build = for_paths(&root, &[root.join("sub/a.txt")], &["build".into()], &l);
        assert!(
            build.contains("[instructions for this path: sub/AGENTS.md:2-2]\nbuild rule")
                && !build.contains("review"),
            "{build}"
        );
        assert_eq!(for_paths(&root, &[root.join("sub/a.txt")], &["build".into()], &l), "");
        let both = for_paths(&root, &[root.join("sub/a.txt")], &["build".into(), "review".into()], &l);
        assert!(
            both.contains("review rule") && !both.contains("build rule") && !both.contains("#zazzle"),
            "{both}"
        );
        assert_eq!(for_paths(&root, &[root.join("other/b.txt")], &["build".into()], &l), "");
        assert!(for_paths(&root, &[root.join("other/b.txt")], &["deploy".into()], &l)
            .contains("only for deploys"));
        assert_eq!(for_paths(&root, &[root.join("blank/AGENTS.md")], &[], &l), "");
    }

    // Checks get-rules with a purpose makes it the session's active purpose for the instructions added to later reads.
    #[test]
    fn test_session_purpose() {
        let t = TempDir::new();
        let root = t.path().canonicalize().expect("root");
        std::fs::create_dir_all(root.join(".git")).expect("git");
        t.write(
            "sub/AGENTS.md",
            b"<!-- #zazzle-purpose-begin:build -->\nbuild rule\n<!-- #zazzle-purpose-end:build -->\n<!-- #zazzle-purpose-begin:review -->\nreview rule\n<!-- #zazzle-purpose-end:review -->\n",
        );
        t.write("sub/a.txt", b"x\n");
        let e = crate::engine::Engine::new();
        crate::testutil::run_cmd_with(&e, &root, "s1", &["get-rules", "x.txt", "--purpose", "review"]);
        let o = crate::testutil::run_cmd_with(&e, &root, "s1", &["read", "sub/a.txt"]);
        assert!(o.notes.contains("review rule") && !o.notes.contains("build rule"), "{}", o.notes);
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
