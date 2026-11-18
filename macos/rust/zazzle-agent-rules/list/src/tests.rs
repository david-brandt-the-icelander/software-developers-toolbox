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
    let f = Found { kind: "rule", path: temp("bad").join("none.md"), lines: Vec::new(), text: None };
    assert!(body(&f, false).contains("error:"));
}

// A skill prints only its description by default and its whole text with skill bodies; a marked segment prints without marker lines.
#[test]
fn test_body_skill_description_and_segment_markers() {
    let root = temp("skill");
    let skill = root.join("agents/skills/s/SKILL.md");
    put(&skill);
    fs::write(&skill, "---\nname: s\ndescription: Does s things.\n---\n\n# s\nlong text\n")
        .expect("writable");
    let dir = root.to_str().expect("utf8");
    let (short, _) = run(&strs(&["get-rules", dir]), None);
    assert_eq!(short, format!("==> skill {} <==\ndescription: Does s things.\n\n", skill.display()));
    let (full, _) = run(&strs(&["get-rules", dir, "--skill-bodies"]), None);
    assert!(full.contains("long text"));
    let rule = root.join("AGENTS.md");
    fs::write(&rule, "#zazzle-purpose-begin:a\nkeep\n#zazzle-purpose-begin:b\nboth\n#zazzle-purpose-end:b\n#zazzle-purpose-end:a\n").expect("writable");
    let (text, _) = run(&strs(&["get-rules", dir, "--purpose", "a", "--purpose", "b"]), None);
    assert!(text.contains("keep\nboth\n"));
    assert!(!text.contains("#zazzle-purpose"));
    let nodesc =
        Found { kind: "skill", path: rule, lines: Vec::new(), text: Some("no front matter".to_string()) };
    assert_eq!(body(&nodesc, false).lines().nth(1), Some(""));
}

// A file that only imports listed files, or is a symlink to one, is not printed again.
#[test]
fn test_list_rules_dedupes_imports_and_symlinks() {
    let root = temp("dedupe");
    fs::write(root.join("AGENTS.md"), "rules\n").expect("writable");
    fs::write(root.join("CLAUDE.md"), "@AGENTS.md\n").expect("writable");
    let got = list_rules(&root, None, &[]).expect("lists");
    assert_eq!(got.len(), 1);
    fs::write(root.join("CLAUDE.md"), "@AGENTS.md\nextra\n").expect("writable");
    assert_eq!(list_rules(&root, None, &[]).expect("lists").len(), 2);
    fs::write(root.join("CLAUDE.md"), "@missing.md\n").expect("writable");
    assert_eq!(list_rules(&root, None, &[]).expect("lists").len(), 2);
    fs::write(root.join("CLAUDE.md"), "\n").expect("writable");
    assert_eq!(list_rules(&root, None, &[]).expect("lists").len(), 2);
    fs::remove_file(root.join("CLAUDE.md")).expect("removable");
    std::os::unix::fs::symlink(root.join("AGENTS.md"), root.join("CLAUDE.md")).expect("symlink");
    assert_eq!(list_rules(&root, None, &[]).expect("lists").len(), 1);
    fs::remove_file(root.join("CLAUDE.md")).expect("removable");
    std::os::unix::fs::symlink(root.join("CLAUDE.md"), root.join("CLAUDE.md")).expect("symlink");
    assert_eq!(list_rules(&root, None, &[]).expect("lists").len(), 1);
}

// Bad --purpose values and unknown flags are usage errors.
#[test]
fn test_run_rejects_bad_options() {
    let root = temp("badopt");
    let dir = root.to_str().expect("utf8");
    assert_eq!(run(&strs(&["get-rules", dir, "--purpose", "--name-only"]), None).1, 2);
    assert_eq!(run(&strs(&["get-rules", dir, "--purpose="]), None).1, 2);
    assert_eq!(run(&strs(&["get-rules", dir, "--bogus"]), None).1, 2);
}

// When the path is under the home folder, home keeps its place in the folder order instead of coming first.
#[test]
fn test_chain_home_inside_ancestors() {
    let root = temp("order-home");
    let home = root.join("h");
    let start = home.join("p");
    fs::create_dir_all(&start).expect("folders");
    assert_eq!(chain(&start, Some(&home)), chain(&start, None));
    let other = root.join("o");
    fs::create_dir_all(&other).expect("folder");
    assert_eq!(chain(&other, Some(&home))[0], home);
}

// The text read for purpose filtering is reused when printing.
#[test]
fn test_body_uses_text_already_read() {
    let f = Found {
        kind: "rule",
        path: PathBuf::from("/nonexistent/x.md"),
        lines: Vec::new(),
        text: Some("kept\n".to_string()),
    };
    assert_eq!(body(&f, false), "==> rule /nonexistent/x.md <==\nkept\n\n");
}

// Folders and non-markdown files in the rules folder, and files that are not text, are handled without failing.
#[test]
fn test_list_rules_odd_files() {
    let root = temp("odd");
    fs::create_dir_all(root.join(".claude/rules/dir.md")).expect("folder");
    fs::write(root.join("AGENTS.md"), [0xff, 0xfe, 0x00]).expect("writable");
    let got = list_rules(&root, None, &[]).expect("lists");
    assert_eq!(got.len(), 1);
    let kept = list_rules(&root, None, &strs(&["a"])).expect("lists");
    assert_eq!(kept.len(), 1);
    assert!(kept[0].text.is_none());
}

// An import of the file itself, or of an existing file that is not listed, does not make a file redundant.
#[test]
fn test_list_rules_import_of_self_or_unlisted() {
    let root = temp("selfimp");
    fs::write(root.join("CLAUDE.md"), "@CLAUDE.md\n").expect("writable");
    assert_eq!(list_rules(&root, None, &[]).expect("lists").len(), 1);
    fs::write(root.join("CLAUDE.md"), "@notes.txt\n").expect("writable");
    fs::write(root.join("notes.txt"), "n").expect("writable");
    assert_eq!(list_rules(&root, None, &[]).expect("lists").len(), 1);
}

// Every spelling of help prints the usage and exits 0.
#[test]
fn test_run_help_spellings() {
    for h in ["help", "--help", "-h"] {
        assert_eq!(run(&strs(&[h]), None).1, 0);
    }
}
