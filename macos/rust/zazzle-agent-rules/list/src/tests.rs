// Tests for list-rules, run against throwaway folder trees.
use super::*;

// Makes a unique empty folder under the system temp folder.
fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zalr-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp folder is creatable");
    fs::canonicalize(&dir).expect("temp folder exists")
}

// Writes a file, creating its folders.
fn put(path: &Path) {
    fs::create_dir_all(path.parent().expect("has parent")).expect("folder is creatable");
    fs::write(path, "x").expect("file is writable");
}

// Strings from literals.
fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// Rules and skills in parent folders are found, farthest first.
#[test]
fn test_list_rules_parents_in_order() {
    let root = temp("order");
    put(&root.join("AGENTS.md"));
    put(&root.join("agents/skills/a/SKILL.md"));
    put(&root.join("sub/CLAUDE.md"));
    put(&root.join("sub/deep/file.rs"));
    let got = list_rules(&root.join("sub/deep/file.rs"), None).expect("lists");
    let paths: Vec<_> = got.iter().map(|f| (f.kind, f.path.clone())).collect();
    assert_eq!(
        paths,
        vec![
            ("rule", root.join("AGENTS.md")),
            ("skill", root.join("agents/skills/a/SKILL.md")),
            ("rule", root.join("sub/CLAUDE.md"))
        ]
    );
}

// Files in sibling folders and below the path do not apply.
#[test]
fn test_list_rules_ignores_siblings_and_children() {
    let root = temp("sib");
    put(&root.join("other/AGENTS.md"));
    put(&root.join("here/inner/AGENTS.md"));
    fs::create_dir_all(root.join("here")).expect("folder");
    assert!(list_rules(&root.join("here"), None)
        .expect("lists")
        .iter()
        .all(|f| !f.path.starts_with(root.join("other")) && !f.path.starts_with(root.join("here/inner"))));
}

// Extra rule folders and hidden skill folders are read, and non-markdown files are skipped.
#[test]
fn test_list_rules_rule_dirs_and_hidden_skills() {
    let root = temp("dirs");
    put(&root.join(".claude/rules/b.md"));
    put(&root.join(".claude/rules/a.md"));
    put(&root.join(".claude/rules/skip.txt"));
    put(&root.join(".claude/skills/s/SKILL.md"));
    put(&root.join(".claude/skills/empty/readme.md"));
    let got = list_rules(&root, None).expect("lists");
    let names: Vec<_> = got
        .iter()
        .map(|f| f.path.strip_prefix(&root).expect("under root").to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![".claude/rules/a.md", ".claude/rules/b.md", ".claude/skills/s/SKILL.md"]);
}

// The home folder comes first, once, whether or not the path is under it.
#[test]
fn test_list_rules_home_first_without_duplicates() {
    let root = temp("home");
    let home = root.join("home");
    put(&home.join(".claude/CLAUDE.md"));
    put(&home.join(".codex/AGENTS.md"));
    put(&home.join("proj/AGENTS.md"));
    let under = list_rules(&home.join("proj"), Some(&home)).expect("lists");
    let kinds: Vec<_> = under
        .iter()
        .map(|f| f.path.strip_prefix(&home).expect("under home").to_string_lossy().into_owned())
        .collect();
    assert_eq!(kinds, vec![".claude/CLAUDE.md", ".codex/AGENTS.md", "proj/AGENTS.md"]);
    put(&root.join("elsewhere/x"));
    let outside = list_rules(&root.join("elsewhere"), Some(&home)).expect("lists");
    assert_eq!(outside.len(), 2);
}

// A missing home folder is ignored and a missing path is an error.
#[test]
fn test_list_rules_missing_inputs() {
    let root = temp("missing");
    assert!(list_rules(&root, Some(&root.join("nohome"))).is_ok());
    assert!(list_rules(&root.join("nope"), None).is_err());
}

// An unreadable or absent folder has no children.
#[test]
fn test_children_missing_folder_is_empty() {
    assert!(children(&temp("kids").join("none")).is_empty());
}

// The command line prints lines, usage or an error with the matching exit code.
#[test]
fn test_run_outputs_and_codes() {
    let root = temp("run");
    put(&root.join("AGENTS.md"));
    let (text, code) = run(&strs(&["list-rules", root.to_str().expect("utf8")]), None);
    assert_eq!((text, code), (format!("rule {}\n", root.join("AGENTS.md").display()), 0));
    assert_eq!(run(&strs(&["--help"]), None).1, 0);
    assert_eq!(run(&strs(&[]), None).1, 2);
    assert_eq!(run(&strs(&["other", "x"]), None).1, 2);
    let (text, code) = run(&strs(&["list-rules", root.join("nope").to_str().expect("utf8")]), None);
    assert_eq!(code, 66);
    assert!(text.contains("nope"));
}
