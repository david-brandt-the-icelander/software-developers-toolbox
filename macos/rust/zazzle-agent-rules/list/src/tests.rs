// Tests for get-rules, run against throwaway folder trees.
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
    let got = list_rules(&root.join("sub/deep/file.rs"), None, &[]).expect("lists");
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
    assert!(list_rules(&root.join("here"), None, &[])
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
    let got = list_rules(&root, None, &[]).expect("lists");
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
    let under = list_rules(&home.join("proj"), Some(&home), &[]).expect("lists");
    let kinds: Vec<_> = under
        .iter()
        .map(|f| f.path.strip_prefix(&home).expect("under home").to_string_lossy().into_owned())
        .collect();
    assert_eq!(kinds, vec![".claude/CLAUDE.md", ".codex/AGENTS.md", "proj/AGENTS.md"]);
    put(&root.join("elsewhere/x"));
    let outside = list_rules(&root.join("elsewhere"), Some(&home), &[]).expect("lists");
    assert_eq!(outside.len(), 2);
}

// A missing home folder is ignored and a missing path is an error.
#[test]
fn test_list_rules_missing_inputs() {
    let root = temp("missing");
    assert!(list_rules(&root, Some(&root.join("nohome")), &[]).is_ok());
    assert!(list_rules(&root.join("nope"), None, &[]).is_err());
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
    let (text, code) = run(&strs(&["get-rules", root.to_str().expect("utf8"), "--name-only"]), None);
    assert_eq!((text, code), (format!("rule {}\n", root.join("AGENTS.md").display()), 0));
    assert_eq!(run(&strs(&["--help"]), None).1, 0);
    assert!(run(&strs(&["--version"]), None).0.starts_with("zazzle-agent "));
    assert_eq!(run(&strs(&[]), None).1, 2);
    assert_eq!(run(&strs(&["other", "x"]), None).1, 2);
    let (text, code) = run(&strs(&["get-rules", root.join("nope").to_str().expect("utf8")]), None);
    assert_eq!(code, 66);
    assert!(text.contains("nope"));
}

// With purposes, unmarked files stay, other-purpose files go, and partly marked files give line ranges.
#[test]
fn test_list_rules_purposes_filter() {
    let root = temp("purpose");
    put(&root.join("AGENTS.md"));
    fs::write(root.join("CLAUDE.md"), "head\n#zazzle-purpose-begin:commit\nc1\n#zazzle-purpose-end:commit\n")
        .expect("writable");
    put(&root.join(".claude/rules/t.md"));
    fs::write(root.join(".claude/rules/t.md"), "#zazzle-purpose:test\n").expect("writable");
    let wanted = strs(&["commit"]);
    let got = list_rules(&root, None, &wanted).expect("lists");
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].lines, vec![(3, 3)]);
    assert_eq!(list_rules(&root, None, &strs(&["test"])).expect("lists").len(), 2);
}

// The command line takes --purpose repeatedly, in both spellings, and prints line ranges.
#[test]
fn test_run_purpose_arguments() {
    let root = temp("runp");
    fs::write(root.join("AGENTS.md"), "#zazzle-purpose-begin:a\n1\n#zazzle-purpose-end:a\n#zazzle-purpose-begin:b\n2\n#zazzle-purpose-end:b\n").expect("writable");
    let dir = root.to_str().expect("utf8");
    let (text, code) = run(&strs(&["get-rules", dir, "--purpose", "a", "--purpose=b", "--name-only"]), None);
    let file = root.join("AGENTS.md");
    assert_eq!((text, code), (format!("rule {0}:2-2\nrule {0}:5-5\n", file.display()), 0));
    assert_eq!(run(&strs(&["get-rules", dir, "--purpose"]), None).1, 2);
    assert_eq!(run(&strs(&["get-rules", dir, dir]), None).1, 2);
    assert_eq!(run(&strs(&["get-rules"]), None).1, 2);
}

// By default the file text follows a header, whole for plain files and only the marked lines for ranges.
#[test]
fn test_run_prints_bodies() {
    let root = temp("body");
    fs::write(root.join("AGENTS.md"), "one\ntwo\n").expect("writable");
    fs::write(root.join("CLAUDE.md"), "x\n#zazzle-purpose-begin:a\nkeep\n#zazzle-purpose-end:a\n")
        .expect("writable");
    let dir = root.to_str().expect("utf8");
    let (text, _) = run(&strs(&["get-rules", dir]), None);
    assert!(text.starts_with(&format!("==> rule {} <==\none\ntwo\n\n", root.join("AGENTS.md").display())));
    let (text, _) = run(&strs(&["get-rules", dir, "--purpose", "a"]), None);
    assert!(text.contains(&format!("==> rule {}:3-3 <==\nkeep\n\n", root.join("CLAUDE.md").display())));
}

// A file that cannot be read as text shows an error in its block.
#[test]
fn test_body_unreadable_file() {
    let f = Found { kind: "rule", path: temp("bad").join("none.md"), lines: Vec::new() };
    assert!(body(&f).contains("error:"));
}
