// Tests for get-rules, run against throwaway folder trees that are also the top folder searched, so files above the temp folder never leak in.
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
    write(path, "x");
}

// Writes a file with the given text, creating its folders.
fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("has parent")).expect("folder is creatable");
    fs::write(path, text).expect("file is writable");
}

// Strings from literals.
fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// Lists the files for a path with no home, searching no higher than root.
fn list(root: &Path, path: &Path, wanted: &[&str]) -> Vec<Found> {
    list_rules(path, None, &strs(wanted), root).expect("lists")
}

// The found paths relative to root.
fn names(root: &Path, found: &[Found]) -> Vec<String> {
    found
        .iter()
        .map(|f| f.path.strip_prefix(root).expect("under root").to_string_lossy().into_owned())
        .collect()
}

// Runs get-rules with arguments, no home and root as the top folder.
fn run_in(root: &Path, args: &[&str]) -> (String, i32) {
    run(&strs(args), None, root)
}

// Rules and skills in parent folders are found, farthest first.
#[test]
fn test_list_rules_parents_in_order() {
    let root = temp("order");
    put(&root.join("AGENTS.md"));
    put(&root.join("agents/skills/a/SKILL.md"));
    put(&root.join("sub/CLAUDE.md"));
    put(&root.join("sub/deep/file.rs"));
    let got = list(&root, &root.join("sub/deep/file.rs"), &[]);
    let paths: Vec<_> = got.iter().map(|f| (f.kind, f.path.clone())).collect();
    assert_eq!(
        paths,
        vec![
            (Kind::Rule, root.join("AGENTS.md")),
            (Kind::Skill, root.join("agents/skills/a/SKILL.md")),
            (Kind::Rule, root.join("sub/CLAUDE.md"))
        ]
    );
}

// Files in sibling folders and below the path do not apply.
#[test]
fn test_list_rules_ignores_siblings_and_children() {
    let root = temp("sib");
    put(&root.join("other/AGENTS.md"));
    put(&root.join("here/inner/AGENTS.md"));
    assert!(list(&root, &root.join("here"), &[]).is_empty());
}

// Rule folders are read at any depth, hidden skill folders are read, and non-markdown files are skipped.
#[test]
fn test_list_rules_rule_dirs_and_hidden_skills() {
    let root = temp("dirs");
    put(&root.join(".claude/rules/b.md"));
    put(&root.join(".claude/rules/a.md"));
    put(&root.join(".claude/rules/front/react.md"));
    put(&root.join(".claude/rules/skip.txt"));
    put(&root.join(".claude/skills/s/SKILL.md"));
    put(&root.join(".claude/skills/empty/readme.md"));
    assert_eq!(
        names(&root, &list(&root, &root, &[])),
        vec![
            ".claude/rules/a.md",
            ".claude/rules/b.md",
            ".claude/rules/front/react.md",
            ".claude/skills/s/SKILL.md"
        ]
    );
}

// A symlink loop inside a rule folder ends at the depth limit.
#[test]
fn test_list_rules_rule_dir_loop_ends() {
    let root = temp("loop");
    put(&root.join(".claude/rules/a.md"));
    std::os::unix::fs::symlink(root.join(".claude/rules"), root.join(".claude/rules/again"))
        .expect("symlink");
    let got = list(&root, &root, &[]);
    assert_eq!(got.len(), 1);
}

// Local and override rule files are found after the files they add to.
#[test]
fn test_list_rules_local_and_override_files() {
    let root = temp("local");
    for name in
        ["CLAUDE.local.md", "AGENTS.override.md", "CLAUDE.md", "AGENTS.md", ".codex/AGENTS.override.md"]
    {
        write(&root.join(name), name);
    }
    assert_eq!(
        names(&root, &list(&root, &root, &[])),
        vec!["AGENTS.md", "AGENTS.override.md", "CLAUDE.md", "CLAUDE.local.md", ".codex/AGENTS.override.md"]
    );
}

// A rule with `paths:` globs applies only to matching files and to folders that may hold them.
#[test]
fn test_list_rules_paths_scope() {
    let root = temp("paths");
    write(&root.join(".claude/rules/docs.md"), "---\npaths: docs/**\n---\nonly docs\n");
    write(&root.join(".claude/rules/ts.md"), "---\npaths:\n  - \"src/**/*.{ts,tsx}\"\n---\nts\n");
    put(&root.join("docs/a.md"));
    put(&root.join("src/x/a.tsx"));
    put(&root.join("src/b.rs"));
    let rules = |p: &str| names(&root, &list(&root, &root.join(p), &[]));
    assert_eq!(rules("docs/a.md"), vec![".claude/rules/docs.md"]);
    assert_eq!(rules("src/x/a.tsx"), vec![".claude/rules/ts.md"]);
    assert!(rules("src/b.rs").is_empty());
    assert_eq!(rules("src"), vec![".claude/rules/ts.md"]);
    assert_eq!(rules("."), vec![".claude/rules/docs.md", ".claude/rules/ts.md"]);
}

// Home rules with `paths:` are matched against the path from `/` when the path is not under home.
#[test]
fn test_list_rules_paths_outside_base() {
    let root = temp("pathsout");
    let home = root.join("home");
    write(&home.join(".claude/rules/rs.md"), "---\npaths: **/*.rs\n---\nrust\n");
    put(&root.join("work/a.rs"));
    put(&root.join("work/a.md"));
    let got = list_rules(&root.join("work/a.rs"), Some(&home), &[], &root).expect("lists");
    assert_eq!(got.len(), 1);
    assert!(list_rules(&root.join("work/a.md"), Some(&home), &[], &root).expect("lists").is_empty());
}

// The home folder comes first, once, whether or not the path is under it.
#[test]
fn test_list_rules_home_first_without_duplicates() {
    let root = temp("home");
    let home = root.join("home");
    put(&home.join(".claude/CLAUDE.md"));
    put(&home.join(".codex/AGENTS.md"));
    put(&home.join("proj/AGENTS.md"));
    let under = list_rules(&home.join("proj"), Some(&home), &[], &root).expect("lists");
    assert_eq!(names(&home, &under), vec![".claude/CLAUDE.md", ".codex/AGENTS.md", "proj/AGENTS.md"]);
    put(&root.join("elsewhere/x"));
    let outside = list_rules(&root.join("elsewhere"), Some(&home), &[], &root).expect("lists");
    assert_eq!(outside.len(), 2);
}

// A missing home folder is ignored and a missing path is an error.
#[test]
fn test_list_rules_missing_inputs() {
    let root = temp("missing");
    assert!(list_rules(&root, Some(&root.join("nohome")), &[], &root).is_ok());
    assert!(list_rules(&root.join("nope"), None, &[], &root).is_err());
}

// A symlinked file gets the rules of the folder it sits in, not of its target's folder.
#[test]
fn test_list_rules_symlinked_file_keeps_its_folder() {
    let root = temp("link");
    put(&root.join("repo/AGENTS.md"));
    put(&root.join("dots/AGENTS.md"));
    put(&root.join("dots/config.md"));
    std::os::unix::fs::symlink(root.join("dots/config.md"), root.join("repo/config.md")).expect("symlink");
    assert_eq!(names(&root, &list(&root, &root.join("repo/config.md"), &[])), vec!["repo/AGENTS.md"]);
}

// Relative paths and the top folder itself resolve to folders.
#[test]
fn test_target_forms() {
    let here = target(Path::new("Cargo.toml")).expect("crate manifest exists");
    assert!(!here.is_dir && here.dir == fs::canonicalize(".").expect("cwd"));
    let top = target(Path::new("/")).expect("root exists");
    assert!(top.is_dir && top.dir == Path::new("/"));
    assert!(target(Path::new("..")).expect("parent exists").is_dir);
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
    let dir = root.to_str().expect("utf8");
    let (text, code) = run_in(&root, &["get-rules", dir, "--name-only"]);
    assert_eq!((text, code), (format!("rule {}\n", root.join("AGENTS.md").display()), 0));
    assert!(run_in(&root, &["--version"]).0.starts_with("zazzle-agent "));
    assert_eq!(run_in(&root, &[]).1, 2);
    assert!(run_in(&root, &["other", "x"]).0.starts_with("unknown command\n"));
    let (text, code) = run_in(&root, &["get-rules", root.join("nope").to_str().expect("utf8")]);
    assert_eq!(code, 66);
    assert!(text.contains("nope"));
}

// With purposes, unmarked files stay, other-purpose files go, and partly marked files give line ranges.
#[test]
fn test_list_rules_purposes_filter() {
    let root = temp("purpose");
    put(&root.join("AGENTS.md"));
    write(&root.join("CLAUDE.md"), "head\n#zazzle-purpose-begin:commit\nc1\n#zazzle-purpose-end:commit\n");
    write(&root.join(".claude/rules/t.md"), "#zazzle-purpose:test\n");
    let got = list(&root, &root, &["commit"]);
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].lines, vec![(3, 3)]);
    assert_eq!(list(&root, &root, &["test"]).len(), 2);
}

// The command line takes --purpose repeatedly, in both spellings, and prints line ranges.
#[test]
fn test_run_purpose_arguments() {
    let root = temp("runp");
    write(
        &root.join("AGENTS.md"),
        "#zazzle-purpose-begin:a\n1\n#zazzle-purpose-end:a\n#zazzle-purpose-begin:b\n2\n#zazzle-purpose-end:b\n",
    );
    let dir = root.to_str().expect("utf8");
    let (text, code) = run_in(&root, &["get-rules", dir, "--purpose", "a", "--purpose=b", "--name-only"]);
    let file = root.join("AGENTS.md");
    assert_eq!((text, code), (format!("rule {0}:2-2\nrule {0}:5-5\n", file.display()), 0));
    assert_eq!(run_in(&root, &["get-rules", dir, "--purpose"]).1, 2);
    assert!(run_in(&root, &["get-rules", dir, dir]).0.starts_with("give one PATH\n"));
    assert!(run_in(&root, &["get-rules"]).0.starts_with("give a PATH\n"));
}

// By default the file text follows a header, whole for plain files and only the marked lines for ranges.
#[test]
fn test_run_prints_bodies() {
    let root = temp("body");
    write(&root.join("AGENTS.md"), "one\ntwo\n");
    write(&root.join("CLAUDE.md"), "x\n#zazzle-purpose-begin:a\nkeep\n#zazzle-purpose-end:a\n");
    let dir = root.to_str().expect("utf8");
    let (text, _) = run_in(&root, &["get-rules", dir]);
    assert!(text.starts_with(&format!("==> rule {} <==\none\ntwo\n\n", root.join("AGENTS.md").display())));
    let (text, _) = run_in(&root, &["get-rules", dir, "--purpose", "a"]);
    assert!(text.contains(&format!("==> rule {}:3-3 <==\nkeep\n\n", root.join("CLAUDE.md").display())));
}

// A file that cannot be read as text shows an error in its block.
#[test]
fn test_body_unreadable_file() {
    let f = Found { kind: Kind::Rule, path: temp("bad").join("none.md"), lines: Vec::new(), text: None };
    assert!(body(&f, false).contains("error:"));
    let root = temp("bad-text");
    write(&root.join("ok.md"), "fine");
    let raced = Found { kind: Kind::Rule, path: root.join("ok.md"), lines: Vec::new(), text: None };
    assert!(body(&raced, false).contains("error: not text"));
}

// A skill prints only its description by default and its whole text with skill bodies; a marked segment prints without marker lines.
#[test]
fn test_body_skill_description_and_segment_markers() {
    let root = temp("skill");
    let skill = root.join("agents/skills/s/SKILL.md");
    write(&skill, "---\nname: s\ndescription: >-\n  Does s\n  things.\n---\n\n# s\nlong text\n");
    let dir = root.to_str().expect("utf8");
    let (short, _) = run_in(&root, &["get-rules", dir]);
    assert_eq!(short, format!("==> skill {} <==\ndescription: Does s things.\n\n", skill.display()));
    let (full, _) = run_in(&root, &["get-rules", dir, "--skill-bodies"]);
    assert!(full.contains("long text"));
    let rule = root.join("AGENTS.md");
    write(&rule, "#zazzle-purpose-begin:a\nkeep\n#zazzle-purpose-begin:b\nboth\n#zazzle-purpose-end:b\n#zazzle-purpose-end:a\n");
    let (text, _) = run_in(&root, &["get-rules", dir, "--purpose", "a", "--purpose", "b"]);
    assert!(text.contains("keep\nboth\n"));
    assert!(!text.contains("#zazzle-purpose"));
    let nodesc =
        Found { kind: Kind::Skill, path: rule, lines: Vec::new(), text: Some("no front matter".to_string()) };
    assert_eq!(body(&nodesc, false).lines().nth(1), Some(""));
}

// A file that only imports listed files, or is a symlink to one, is not printed again.
#[test]
fn test_list_rules_dedupes_imports_and_symlinks() {
    let root = temp("dedupe");
    write(&root.join("AGENTS.md"), "rules\n");
    write(&root.join("CLAUDE.md"), "@AGENTS.md\n");
    assert_eq!(list(&root, &root, &[]).len(), 1);
    write(&root.join("CLAUDE.md"), "@AGENTS.md\nextra\n");
    assert_eq!(list(&root, &root, &[]).len(), 2);
    write(&root.join("CLAUDE.md"), "@missing.md\n");
    assert_eq!(list(&root, &root, &[]).len(), 2);
    write(&root.join("CLAUDE.md"), "\n");
    assert_eq!(list(&root, &root, &[]).len(), 2);
    fs::remove_file(root.join("CLAUDE.md")).expect("removable");
    std::os::unix::fs::symlink(root.join("AGENTS.md"), root.join("CLAUDE.md")).expect("symlink");
    assert_eq!(list(&root, &root, &[]).len(), 1);
    fs::remove_file(root.join("CLAUDE.md")).expect("removable");
    std::os::unix::fs::symlink(root.join("CLAUDE.md"), root.join("CLAUDE.md")).expect("symlink");
    assert_eq!(list(&root, &root, &[]).len(), 1);
}

// Imported files follow the rule that imports them, once each, nested up to the depth limit, and are narrowed by purpose.
#[test]
fn test_list_rules_follows_imports() {
    let root = temp("imports");
    write(&root.join("CLAUDE.md"), "Read @docs/a.md and @docs/a.md\n");
    write(&root.join("AGENTS.md"), "top\n");
    write(&root.join("docs/a.md"), "@b.md\n@../AGENTS.md\n");
    write(&root.join("docs/b.md"), "@c.md\n");
    for (name, next) in [("c", "d"), ("d", "e"), ("e", "f"), ("f", "g")] {
        write(&root.join(format!("docs/{name}.md")), &format!("@{next}.md\n"));
    }
    put(&root.join("docs/g.md"));
    let got = list(&root, &root, &[]);
    let kinds: Vec<_> = got.iter().map(|f| f.kind.name()).collect();
    assert_eq!(kinds, vec!["rule", "rule", "import", "import", "import", "import", "import"]);
    assert_eq!(
        names(&root, &got),
        vec!["AGENTS.md", "CLAUDE.md", "docs/a.md", "docs/b.md", "docs/c.md", "docs/d.md", "docs/e.md"]
    );
    write(&root.join("docs/a.md"), "#zazzle-purpose:other\n@b.md\n");
    assert_eq!(names(&root, &list(&root, &root, &["x"])), vec!["AGENTS.md", "CLAUDE.md"]);
    let (text, _) = run_in(&root, &["get-rules", root.to_str().expect("utf8"), "--name-only"]);
    assert!(text.contains(&format!("import {}\n", root.join("docs/a.md").display())));
}

// Only the applying lines of a narrowed rule are searched for imports, and skills are never expanded.
#[test]
fn test_list_rules_imports_of_selected_lines_only() {
    let root = temp("impsel");
    write(&root.join("AGENTS.md"), "@out.md\n#zazzle-purpose-begin:a\n@in.md\n#zazzle-purpose-end:a\n");
    put(&root.join("out.md"));
    put(&root.join("in.md"));
    write(&root.join("agents/skills/s/SKILL.md"), "@out.md\n");
    let got = list(&root, &root, &["a"]);
    assert_eq!(names(&root, &got), vec!["AGENTS.md", "in.md", "agents/skills/s/SKILL.md"]);
}

// Bad --purpose values and unknown flags are usage errors that say what is wrong.
#[test]
fn test_run_rejects_bad_options() {
    let root = temp("badopt");
    let dir = root.to_str().expect("utf8");
    assert_eq!(run_in(&root, &["get-rules", dir, "--purpose", "--name-only"]).1, 2);
    assert_eq!(run_in(&root, &["get-rules", dir, "--purpose="]).1, 2);
    let (text, code) = run_in(&root, &["get-rules", dir, "--purpose", "commit-"]);
    assert!(code == 2 && text.contains("`commit-`"));
    assert_eq!(run_in(&root, &["get-rules", dir, "--purpose", "a b"]).1, 2);
    assert!(run_in(&root, &["get-rules", dir, "--bogus"]).0.starts_with("unknown option --bogus\n"));
}

// When the path is under the home folder, home keeps its place in the folder order instead of coming first.
#[test]
fn test_chain_home_inside_ancestors() {
    let root = temp("order-home");
    let home = root.join("h");
    let start = home.join("p");
    fs::create_dir_all(&start).expect("folders");
    assert_eq!(chain(&start, Some(&home), &root), chain(&start, None, &root));
    assert_eq!(chain(&start, None, &root), vec![root.clone(), home.clone(), start.clone()]);
    let other = root.join("o");
    fs::create_dir_all(&other).expect("folder");
    assert_eq!(chain(&other, Some(&home), &root)[0], home);
}

// The text read when a file is found is used when printing.
#[test]
fn test_body_uses_text_already_read() {
    let f = Found {
        kind: Kind::Rule,
        path: PathBuf::from("/nonexistent/x.md"),
        lines: Vec::new(),
        text: Some("kept\n".to_string()),
    };
    assert_eq!(body(&f, false), "==> rule /nonexistent/x.md <==\nkept\n\n");
}

// Folders named like markdown in the rules folder, and files that are not text, are handled without failing.
#[test]
fn test_list_rules_odd_files() {
    let root = temp("odd");
    fs::create_dir_all(root.join(".claude/rules/dir.md")).expect("folder");
    fs::write(root.join("AGENTS.md"), [0xff, 0xfe, 0x00]).expect("writable");
    assert_eq!(list(&root, &root, &[]).len(), 1);
    let kept = list(&root, &root, &["a"]);
    assert_eq!(kept.len(), 1);
    assert!(kept[0].text.is_none());
}

// An import of the file itself, or of an existing file that is not listed, does not make a file redundant.
#[test]
fn test_list_rules_import_of_self_or_unlisted() {
    let root = temp("selfimp");
    write(&root.join("CLAUDE.md"), "@CLAUDE.md\n");
    assert_eq!(list(&root, &root, &[]).len(), 1);
    write(&root.join("CLAUDE.md"), "@notes.txt\n");
    write(&root.join("notes.txt"), "n");
    assert_eq!(names(&root, &list(&root, &root, &[])), vec!["CLAUDE.md", "notes.txt"]);
}

// Every spelling of help prints the usage and exits 0, alone or after get-rules.
#[test]
fn test_run_help_spellings() {
    let top = Path::new("/");
    for args in [
        &["help"][..],
        &["--help"],
        &["-h"],
        &["help", "get-rules"],
        &["get-rules", "--help"],
        &["get-rules", ".", "-h"],
    ] {
        assert_eq!(run(&strs(args), None, top), (USAGE.to_string(), 0));
    }
    assert_eq!(run(&strs(&["get-rules", "help"]), None, top).1, 66);
}
