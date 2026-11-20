// Tests for the repository workflow on real git repositories in temporary folders, with scripted agents and a stand-in gh: policy and discovery, receipts, the writer, delivery and its retries, CI waiting, test-first and the multi-viewpoint review with remediation.
use super::*;
use crate::adaptive::agent::Said;
use crate::llm::Reply;
use crate::testutil::TempDir;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

// One scripted turn: a reply, optionally after writing files (a hosted agent's edit).
#[derive(Clone)]
struct Turn {
    text: Result<String, String>,
    files: Vec<(String, String)>,
}

// A scripted turn that only replies.
fn say(t: &str) -> Turn {
    Turn { text: Ok(t.into()), files: Vec::new() }
}

// A scripted turn that edits files, then replies.
fn edits(files: &[(&str, &str)], t: &str) -> Turn {
    Turn { text: Ok(t.into()), files: files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect() }
}

// A scripted turn that fails.
fn fails(e: &str) -> Turn {
    Turn { text: Err(e.into()), files: Vec::new() }
}

// Agents that reply from a script per rung label, writing a hosted turn's files into its working folder, and record each prompt.
#[derive(Default)]
struct Fake {
    turns: Mutex<HashMap<String, VecDeque<Turn>>>,
    seen: Mutex<Vec<(String, String)>>,
    installed: Vec<String>,
}

impl Fake {
    // Builds a script from (rung label, turn) pairs in the order each rung is asked.
    fn new(script: Vec<(&str, Turn)>) -> Fake {
        let f = Fake::default();
        for (who, t) in script {
            f.turns.lock().expect("lock").entry(who.to_string()).or_default().push_back(t);
        }
        f
    }

    // The next turn for a rung.
    fn next(&self, who: String, prompt: String) -> Turn {
        self.seen.lock().expect("lock").push((who.clone(), prompt));
        let mut t = self.turns.lock().expect("lock");
        t.get_mut(&who)
            .and_then(|q| q.pop_front())
            .unwrap_or_else(|| fails(&format!("no turn scripted for {who}")))
    }

    // The prompts one rung was given.
    fn prompts(&self, who: &str) -> Vec<String> {
        self.seen.lock().expect("lock").iter().filter(|(w, _)| w == who).map(|(_, p)| p.clone()).collect()
    }
}

impl Backend for Fake {
    // Replies for `MODEL EFFORT`.
    fn local(&self, model: &str, req: &Value, _d: Instant) -> Result<Reply, String> {
        let r = Rung {
            kind: agent::Kind::Local,
            model: model.into(),
            effort: req["effort"].as_str().unwrap_or_default().into(),
        };
        let prompt = format!(
            "{}\n{}",
            req["system"].as_str().unwrap_or_default(),
            req["user"].as_str().unwrap_or_default()
        );
        self.next(r.label(), prompt).text.map(|text| Reply {
            text,
            truncated: false,
            tokens: 5,
            queued_ms: 100,
        })
    }

    // Writes the turn's files when allowed to, then replies.
    fn hosted(
        &self,
        rung: &Rung,
        prompt: &str,
        cwd: &Path,
        _d: Instant,
        write: bool,
    ) -> Result<Said, String> {
        let t = self.next(rung.label(), prompt.into());
        if write {
            for (p, c) in &t.files {
                let full = cwd.join(p);
                std::fs::create_dir_all(full.parent().expect("folder")).expect("mkdir");
                std::fs::write(full, c).expect("write");
            }
        }
        t.text.map(|text| Said { text, tokens: 9, input: 200, cached: 50, rounds: 3, cost: Some(0.02) })
    }

    // The scripted installed models.
    fn installed(&self) -> Vec<String> {
        self.installed.clone()
    }
}

// Runs git in a folder, panicking on failure.
fn g(dir: &Path, args: &[&str]) -> String {
    repo::git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

// A repository on main with one commit (a Rust-looking source and a test script), then a feature branch.
fn repo_on_feature() -> (TempDir, PathBuf) {
    let t = TempDir::new();
    let root = t.path().canonicalize().expect("root");
    g(&root, &["init", "-q", "-b", "main"]);
    g(&root, &["config", "user.email", "t@t"]);
    g(&root, &["config", "user.name", "t"]);
    g(&root, &["config", "commit.gpgsign", "false"]);
    t.write("src/lib.rs", b"pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n");
    t.write("tests/check.sh", b"exit 0\n");
    t.write("README.md", b"# demo\n");
    g(&root, &["add", "-A"]);
    g(&root, &["commit", "-qm", "start"]);
    g(&root, &["checkout", "-qb", "feature/x"]);
    (t, root)
}

// Writes the opt-in file with one check and commits it on main, which the feature branch then has too.
fn opt_in(t: &TempDir, extra: &str) {
    let cfg = format!("{{\"checks\": [{{\"name\": \"unit\", \"run\": \"sh tests/check.sh\", \"kind\": \"test\"}}], \"branches\": {{\"target\": \"main\"}}{extra}}}");
    let root = t.path();
    let back = g(root, &["symbolic-ref", "--short", "HEAD"]);
    g(root, &["checkout", "-q", "main"]);
    t.write(".zazzle/flow.json", cfg.as_bytes());
    g(root, &["add", ".zazzle/flow.json"]);
    g(root, &["commit", "-qm", "opt in"]);
    g(root, &["checkout", "-q", &back]);
    g(root, &["merge", "-q", "main"]);
}

// Runs `flow ARGS` in a folder with the fake agents.
fn flow(root: &Path, f: &Fake, args: &[&str]) -> (String, i32) {
    run(root, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), f)
}

// Writes an executable script.
fn exe(t: &TempDir, name: &str, body: &str) -> PathBuf {
    let p = t.write(name, format!("#!/bin/sh\n{body}").as_bytes());
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    p
}

// Checks discovery across project kinds, skipping folders inside a Rust workspace and build folders, without running anything.
#[test]
fn test_discover() {
    let t = TempDir::new();
    t.write("Cargo.toml", b"[workspace]\n");
    t.write("crates/a/Cargo.toml", b"[package]\n");
    t.write("web/package.json", br#"{"scripts": {"test": "jest"}}"#);
    t.write("web/node_modules/x/package.json", br#"{"scripts": {"test": "x"}}"#);
    t.write("docs/package.json", br#"{"scripts": {}}"#);
    t.write("svc/go.mod", b"module x\n");
    t.write("py/pyproject.toml", b"");
    t.write("cs/App.sln", b"");
    t.write("c/Makefile", b"all:\ntest:\n\tcc t.c\n");
    t.write("d/Makefile", b"all:\n");
    let names: Vec<(String, String)> =
        policy::discover(t.path()).into_iter().map(|c| (c.name, c.run)).collect();
    assert_eq!(
        names,
        [
            ("cargo".to_string(), "cargo test --workspace".to_string()),
            ("make c".into(), "make test".into()),
            ("dotnet cs".into(), "dotnet test App.sln".into()),
            ("pytest py".into(), "python3 -m pytest -q".into()),
            ("go svc".into(), "go test ./...".into()),
            ("npm web".into(), "npm test".into()),
        ]
    );
    let sub = TempDir::new();
    sub.write("rust/Cargo.toml", b"");
    sub.write("rust/inner/Cargo.toml", b"");
    let found = policy::discover(sub.path());
    assert_eq!((found.len(), found[0].paths.clone()), (1, vec!["rust/".to_string()]));
}

// Checks the opt-in file: checks, levels, protection, effects and agents, and each kind of error in it.
#[test]
fn test_policy_file() {
    let (t, root) = repo_on_feature();
    opt_in(&t, ", \"agents\": {\"writer\": \"qwen-a\", \"reviewers\": [\"claude:x\", \"codex:y\"], \"fix\": \"claude:x@high\"}, \"effects\": [\"commit\"]");
    let p = policy::load(&root).expect("policy");
    assert!(p.opted_in && p.effects == ["commit"] && p.target == "main" && p.target_from == policy::FILE);
    assert_eq!(
        (
            p.agent("writer").as_deref(),
            p.agent_list("reviewers").len(),
            p.agent_list("fix").len(),
            p.agent_list("none").len()
        ),
        (Some("qwen-a"), 2, 1, 0)
    );
    assert_eq!(
        (p.level("main"), p.level("staging"), p.level("dev"), p.level("feature/x")),
        ("production".into(), "staging".into(), "development".into(), "feature".into())
    );
    assert!(p.is_protected("main") && !p.is_protected("feature/x"));
    let bad = |cfg: &str| {
        t.write(".zazzle/flow.json", cfg.as_bytes());
        policy::load(&root).unwrap_err()
    };
    assert!(bad("{").contains(".zazzle/flow.json"));
    assert!(bad(r#"{"checks": [{"name": "x"}]}"#).contains("has no `run`"));
    assert!(bad(r#"{"checks": [{"run": "x", "kind": "lint"}]}"#).contains("build, test, coverage or script"));
    assert!(bad(r#"{"branches": {"levels": {"qa": "uat"}}}"#).contains("branches.levels.qa"));
    assert!(bad(r#"{"effects": ["deploy"]}"#).contains("not `deploy`"));
    t.write(".zazzle/flow.json", br#"{"branches": {"levels": {"qa": "staging"}, "protected": ["release"]}}"#);
    let p = policy::load(&root).expect("levels");
    assert_eq!(
        (p.level("qa"), p.is_protected("release"), p.effects.len(), p.checks.len()),
        ("staging".into(), true, 3, 0)
    );
    assert_eq!(p.target, "main", "no target: the default branch");
}

// Checks the guidance's statements about pull request targets: one that differs from the default sets the target when not opted in, two disagreeing are a conflict, and an opted-in file resolves or contradicts them.
#[test]
fn test_policy_conflicts() {
    let (t, root) = repo_on_feature();
    t.write("AGENTS.md", b"# Rules\nFeature PRs target `dev`.\nNothing else here.\n");
    let p = policy::load(&root).expect("one statement");
    assert_eq!((p.target.as_str(), p.conflicts.len()), ("dev", 0));
    assert!(p.target_from.contains("AGENTS.md:2 says dev"), "{}", p.target_from);
    t.write("skills/repo-governance/SKILL.md", b"Open the pull request against `staging`.\n");
    let p = policy::load(&root).expect("two statements");
    assert!(
        p.conflicts[0].contains("AGENTS.md:2 says dev; skills/repo-governance/SKILL.md:1 says staging")
            && p.conflicts[0].contains("set branches.target"),
        "{:?}",
        p.conflicts
    );
    opt_in(&t, "");
    let p = policy::load(&root).expect("resolved");
    assert!(p.conflicts[0].contains("resolves it: main"), "{:?}", p.conflicts);
    std::fs::remove_file(root.join("skills/repo-governance/SKILL.md")).expect("rm");
    let p = policy::load(&root).expect("contradicted");
    assert!(
        p.conflicts[0].contains("the guidance says pull requests target dev")
            && p.conflicts[0].contains("says main"),
        "{:?}",
        p.conflicts
    );
    let solo = TempDir::new();
    g(solo.path(), &["init", "-q", "-b", "trunk"]);
    assert_eq!(policy::load(solo.path()).expect("empty").target, "main", "no main, master or origin: main");
}

// Checks the git helpers: the working tree's tree includes untracked files without staging them, changes, restore, prefixes, bases and errors.
#[test]
fn test_repo_helpers() {
    let (t, root) = repo_on_feature();
    let clean = repo::worktree_tree(&root).expect("tree");
    assert_eq!(clean, repo::tree_of(&root, "HEAD").expect("head tree"));
    t.write("new.txt", b"x\n");
    let dirty = repo::worktree_tree(&root).expect("dirty");
    t.write("README.md", b"# DEMO\n");
    assert_ne!(
        repo::worktree_tree(&root).expect("same size"),
        dirty,
        "a same-size rewrite in the same second is seen"
    );
    t.write("README.md", b"# demo\n");
    assert_ne!(dirty, clean);
    assert_eq!(g(&root, &["diff", "--cached", "--name-only"]), "", "the real index is untouched");
    assert_eq!(repo::changed(&root, &clean, &dirty).expect("changed"), ["new.txt"]);
    assert!(repo::diff(&root, &clean, &dirty).expect("diff").contains("+x"));
    t.write("src/lib.rs", b"changed\n");
    repo::restore(&root, &clean, &["src/lib.rs".into(), "new.txt".into()]).expect("restore");
    assert!(
        std::fs::read_to_string(root.join("src/lib.rs")).expect("lib").contains("a + b")
            && !root.join("new.txt").exists()
    );
    assert!(
        repo::under("src/a.rs", &["src/".into()])
            && repo::under("src", &["src".into()])
            && repo::under("x", &[".".into()])
            && !repo::under("srcx/a", &["src".into()])
    );
    assert_eq!(repo::base(&root, "main").expect("base"), g(&root, &["rev-parse", "main"]));
    assert!(repo::base(&root, "nope").unwrap_err().contains("no branch `nope`"));
    g(&root, &["checkout", "-q", "--detach"]);
    assert!(repo::branch(&root).unwrap_err().contains("detached"));
    assert!(repo::top(&std::env::temp_dir().join("zazzle-no-repo-here")).is_err());
    assert!(repo::git(&root, &["no-such-command"]).unwrap_err().starts_with("git no-such-command"));
}

// Checks reading test summaries and coverage from each tool's output.
#[test]
fn test_parse_results() {
    let cargo = "test result: ok. 12 passed; 0 failed; 1 ignored; 0 measured\ntest a::b ... FAILED\ntest result: FAILED. 3 passed; 2 failed; 0 ignored\n";
    let (p, f, s, c, fails) = verify::parse(cargo);
    assert_eq!((p, f, s, c, fails), (15, 2, 1, None, vec!["test a::b ... FAILED".to_string()]));
    assert_eq!(verify::parse("Failed!  - Failed:     1, Passed:    92, Skipped:     3, Total: 96").0, 92);
    assert_eq!(verify::parse("Tests:       1 failed, 2 skipped, 10 passed, 13 total").1, 1);
    assert_eq!(
        verify::parse("===== 3 failed, 10 passed, 1 skipped in 2.1s =====\nFAILED tests/t.py::test_x").4,
        ["FAILED tests/t.py::test_x"]
    );
    let go = "ok  \tpkg/a\t0.1s\nFAIL\tpkg/b\n--- FAIL: TestB (0.0s)\n";
    assert_eq!(verify::parse(go).0, 1);
    assert_eq!(verify::parse(go).1, 1);
    assert_eq!(verify::parse("daemon branch coverage: 95.14%\nTOTAL 100 5 95.00%").3, Some(95.0));
    assert_eq!(verify::parse("coverage: none").3, None);
}

// Checks running checks: passed, failed with its last lines, missing program, stopped at the time limit, and a receipt pinned to the tree, read back, and not inherited by a changed tree.
#[test]
fn test_verify_and_receipts() {
    let (t, root) = repo_on_feature();
    let c = |run: &str| policy::Check {
        name: "c".into(),
        run: run.into(),
        dir: ".".into(),
        kind: "test".into(),
        paths: Vec::new(),
    };
    let log = root.join("c.log");
    let ok = verify::run_check(
        &root,
        &c("echo 'test result: ok. 2 passed; 0 failed; 0 ignored'"),
        &log,
        Duration::from_secs(10),
    );
    assert_eq!((ok.status.as_str(), ok.passed), ("passed", 2));
    let bad = verify::run_check(&root, &c("echo one; echo boom; exit 3"), &log, Duration::from_secs(10));
    assert_eq!(
        (bad.status.as_str(), bad.code, bad.failures.clone()),
        ("failed", Some(3), vec!["one".to_string(), "boom".to_string()])
    );
    assert!(bad.summary().starts_with("failed exit 3"));
    let gone = verify::run_check(&root, &c("no-such-tool-zz --x"), &log, Duration::from_secs(10));
    assert_eq!(
        (gone.status.as_str(), gone.summary()),
        ("missing", "not run: `no-such-tool-zz` is not installed".to_string())
    );
    let slow = verify::run_check(&root, &c("sleep 5"), &log, Duration::from_millis(200));
    assert!(slow.status == "failed" && slow.failures[0].contains("stopped after"));
    assert!(slow.summary().contains("exit killed") || slow.summary().starts_with("failed"));
    let cov = verify::run_check(&root, &c("echo 'coverage: 91.5%'"), &log, Duration::from_secs(10));
    assert!(cov.summary().contains("coverage 91.50%"));
    opt_in(&t, "");
    let mut p = policy::load(&root).expect("policy");
    p.checks.push(policy::Check {
        name: "docs".into(),
        run: "true".into(),
        dir: ".".into(),
        kind: "script".into(),
        paths: vec!["docs/".into()],
    });
    let base = g(&root, &["rev-parse", "main"]);
    let r = verify::verify(&root, &p, Some(&base), &[], Duration::from_secs(10)).expect("verify");
    assert!(r.green() && r.checks[1].status == "not applicable");
    assert_eq!(verify::receipt_for(&root, &r.tree), Some(r.clone()));
    let text = r.text();
    assert!(
        text.contains("✓ unit (test): passed") && text.contains("- docs (script): not applicable"),
        "{text}"
    );
    t.write("src/lib.rs", b"changed\n");
    let now = repo::worktree_tree(&root).expect("tree");
    assert!(verify::receipt_for(&root, &now).is_none(), "a changed tree inherits nothing");
    t.write("tests/check.sh", b"echo nope; exit 1\n");
    let red = verify::verify(&root, &p, None, &["unit".into()], Duration::from_secs(10)).expect("red");
    assert!(!red.green() && red.line().contains("unit failed") && red.text().contains("log: "));
    assert_eq!(red.checks.len(), 1, "--only runs the named checks");
    let empty = verify::Receipt::default();
    assert!(!empty.green() && empty.text().contains("no checks found"));
    assert_eq!(verify::short("abc"), "abc");
}

// Checks delivery: refused before opting in, for effects not allowed, on a protected branch, without paths or a message, or with other staged files; then a commit, a retry that makes none, an unverified and a verified commit.
#[test]
fn test_deliver_commit() {
    let (t, root) = repo_on_feature();
    let a = |paths: &[&str], m: &str| deliver::Ask {
        paths: paths.iter().map(|s| s.to_string()).collect(),
        message: m.into(),
        ..deliver::Ask::default()
    };
    let p = policy::load(&root).expect("policy");
    assert!(deliver::deliver(&root, &p, &a(&["src"], "m")).unwrap_err().contains("not opted in"));
    opt_in(&t, ", \"effects\": [\"commit\"]");
    let p = policy::load(&root).expect("policy");
    assert!(deliver::deliver(&root, &p, &deliver::Ask { push: true, ..a(&["src"], "m") })
        .unwrap_err()
        .contains("does not allow `push`"));
    assert!(deliver::deliver(&root, &p, &a(&[], "m")).unwrap_err().contains("--paths"));
    assert!(deliver::deliver(&root, &p, &a(&["src"], " ")).unwrap_err().contains("-m"));
    t.write("src/lib.rs", b"pub fn add(a: i32, b: i32) -> i32 {\n    a.wrapping_add(b)\n}\n");
    t.write("README.md", b"# demo 2\n");
    g(&root, &["add", "README.md"]);
    assert!(deliver::deliver(&root, &p, &a(&["src"], "m"))
        .unwrap_err()
        .contains("README.md is staged but not among --paths"));
    g(&root, &["reset", "-q", "README.md"]);
    assert!(deliver::deliver(
        &root,
        &p,
        &deliver::Ask { require_verified: true, ..a(&["src"], "Use wrapping add") }
    )
    .unwrap_err()
    .contains("no green verification"));
    let out = deliver::deliver(&root, &p, &a(&["src"], "Use wrapping add")).expect("commit");
    assert!(
        out.contains("commit: ")
            && out.contains("(1 files)")
            && out.contains("branch: feature/x (base main)")
            && out.contains("the commit is unverified"),
        "{out}"
    );
    assert_eq!(g(&root, &["status", "--porcelain"]), " M README.md", "only the named paths were committed");
    let again = deliver::deliver(&root, &p, &a(&["src"], "Use wrapping add")).expect("retry");
    assert!(again.contains("already made; not repeated"), "{again}");
    assert_eq!(g(&root, &["rev-list", "--count", "main..HEAD"]), "1", "a retry makes no second commit");
    assert!(deliver::deliver(&root, &p, &a(&["src"], "Other")).unwrap_err().contains("another message"));
    verify::verify(&root, &p, None, &[], Duration::from_secs(10)).expect("verify");
    g(&root, &["checkout", "-q", "README.md"]);
    t.write("src/more.rs", b"// more\n");
    let tree_after = {
        g(&root, &["add", "src/more.rs"]);
        let tr = g(&root, &["write-tree"]);
        g(&root, &["reset", "-q", "src/more.rs"]);
        tr
    };
    let _ = tree_after;
    verify::verify(&root, &p, None, &[], Duration::from_secs(10)).expect("verify more");
    let out =
        deliver::deliver(&root, &p, &deliver::Ask { require_verified: true, ..a(&["src"], "Add more") })
            .expect("verified");
    assert!(out.contains("verification: 1 checks passed on tree"), "{out}");
    g(&root, &["checkout", "-q", "main"]);
    assert!(deliver::deliver(&root, &p, &a(&["src"], "m"))
        .unwrap_err()
        .contains("main is a production branch"));
}

// Checks pushing to a remote and the pull request through a stand-in gh: opened as a draft, found again on a retry with its body updated, the push up to date, a remote ahead reported and never forced, and no origin refused.
#[test]
fn test_deliver_push_and_pr() {
    let _g = crate::testutil::env_lock();
    let (t, root) = repo_on_feature();
    opt_in(&t, "");
    let p = policy::load(&root).expect("policy");
    let ask = |m: &str| deliver::Ask {
        paths: vec!["src".into()],
        message: m.into(),
        push: true,
        pr: true,
        draft: true,
        ..deliver::Ask::default()
    };
    t.write("src/lib.rs", b"// v2\n");
    assert!(deliver::deliver(&root, &p, &ask("v2")).unwrap_err().contains("no `origin` remote"));
    let bare = TempDir::new();
    g(bare.path(), &["init", "-q", "--bare"]);
    g(&root, &["remote", "add", "origin", &bare.path().display().to_string()]);
    g(&root, &["push", "-q", "origin", "main"]);
    let state = t.path().join("gh-state");
    let gh = exe(&t, "gh", &format!("echo \"$@\" >> {s}.log\ncase \"$2\" in\n list) if [ -f {s} ]; then echo '[{{\"number\":7,\"url\":\"https://x/pr/7\",\"isDraft\":true,\"baseRefName\":\"dev\"}}]'; else echo '[]'; fi;;\n create) touch {s}; echo 'https://x/pr/7';;\n edit) echo ok;;\nesac\n", s = state.display()));
    std::env::set_var("ZAZZLE_GH", &gh);
    let first = deliver::deliver(&root, &p, &ask("v2"));
    let body = t.write("body.md", b"## What\nv2\n");
    let second = deliver::deliver(&root, &p, &deliver::Ask { body: Some(body), ..ask("v2") });
    std::env::set_var("ZAZZLE_GH", t.path().join("missing-gh"));
    let no_gh = deliver::run_gh(&root, &["pr", "list"]);
    let bad = exe(&t, "badgh", "echo 'not logged in' >&2\nexit 1\n");
    std::env::set_var("ZAZZLE_GH", &bad);
    let failing = deliver::run_gh(&root, &["pr", "view"]);
    std::env::remove_var("ZAZZLE_GH");
    let first = first.expect("first");
    assert!(
        first.contains("push: pushed to origin/feature/x")
            && first.contains("pr: https://x/pr/7 (opened, draft)"),
        "{first}"
    );
    let second = second.expect("second");
    assert!(
        second.contains("already made")
            && second.contains("push: up to date")
            && second.contains("#7 (already open, draft, body updated); it targets dev, not main"),
        "{second}"
    );
    let calls = std::fs::read_to_string(format!("{}.log", state.display())).expect("gh log");
    assert_eq!(calls.matches("pr create").count(), 1, "a retry opens no second pull request: {calls}");
    assert!(calls.contains("--draft") && calls.contains("--base main --head feature/x"), "{calls}");
    assert!(no_gh.unwrap_err().contains("install the GitHub CLI"));
    assert_eq!(failing.unwrap_err(), "gh pr view: not logged in");
    let other = TempDir::new();
    g(other.path(), &["clone", "-q", &bare.path().display().to_string(), "c"]);
    let c = other.path().join("c");
    g(&c, &["config", "user.email", "t@t"]);
    g(&c, &["config", "user.name", "t"]);
    g(&c, &["checkout", "-q", "feature/x"]);
    std::fs::write(c.join("x.txt"), "x").expect("x");
    g(&c, &["add", "x.txt"]);
    g(&c, &["commit", "-qm", "theirs"]);
    g(&c, &["push", "-q", "origin", "feature/x"]);
    t.write("src/lib.rs", b"// v3\n");
    let ahead = deliver::deliver(&root, &p, &deliver::Ask { pr: false, ..ask("v3") });
    assert!(ahead.unwrap_err().contains("has commits this branch lacks"), "never forced");
}

// Checks the writer: the facts it needs, a valid commit message, a draft fixed on the second try, one still invalid, the template's headings, changelog sections and insertion, and drafts saved by --apply.
#[test]
fn test_write() {
    let (t, root) = repo_on_feature();
    opt_in(&t, "");
    t.write(".github/pull_request_template.md", b"## What changes\n\n## Testing\n");
    t.write("CHANGELOG.md", b"# Changelog\n\n## [Unreleased]\n\n### Fixed\n- old\n");
    g(&root, &["add", "-A"]);
    g(&root, &["commit", "-qm", "templates"]);
    let none = Fake::new(vec![]);
    assert!(flow(&root, &none, &["write", "commit", "--intent", "x"]).0.contains("no change to describe"));
    t.write("src/lib.rs", b"pub fn add(a: i32, b: i32) -> i32 {\n    a.wrapping_add(b)\n}\n");
    assert!(flow(&root, &none, &["write", "commit"]).0.contains("--intent"));
    assert!(flow(&root, &none, &["write", "pr", "--intent", "x"])
        .0
        .contains("compatibility decision is yours"));
    assert!(flow(&root, &none, &["write", "poem"]).0.contains("usage: zazzle flow write"));
    let f =
        Fake::new(vec![("local default low", say("```\nUse wrapping add\n\nAvoids overflow panics.\n```"))]);
    let (out, code) = flow(&root, &f, &["write", "commit", "--intent", "avoid overflow"]);
    assert_eq!((out.as_str(), code), ("Use wrapping add\n\nAvoids overflow panics.", 0));
    let sent = &f.prompts("local default low")[0];
    assert!(
        sent.contains("no verification of tree")
            && sent.contains("a.wrapping_add(b)")
            && sent.contains("Intent: avoid overflow"),
        "{sent}"
    );
    let f = Fake::new(vec![
        ("qwen-w low", say("## What changes\nwrapping add")),
        ("qwen-w low", say("## What changes\nwrapping add\n## Testing\nnone yet")),
    ]);
    let (out, code) = flow(
        &root,
        &f,
        &["write", "pr", "--intent", "x", "--compat", "none", "--writer", "qwen-w", "--apply"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("-- saved to ")
            && f.prompts("qwen-w low")[1].contains("the template heading `## Testing` is missing"),
        "{out}"
    );
    let f = Fake::new(vec![
        ("local default low", say("### Fixd\n- typo")),
        ("local default low", say("- loose line")),
    ]);
    let (out, code) = flow(&root, &f, &["write", "changelog", "--intent", "x", "--compat", "none"]);
    assert_eq!(code, 1);
    assert!(
        out.contains("not valid after a second draft")
            && out.contains("is not under a `### Section` heading"),
        "{out}"
    );
    let f = Fake::new(vec![("local default low", say("### Fixed\n- Adding no longer panics on overflow."))]);
    let (out, _) = flow(&root, &f, &["write", "changelog", "--intent", "x", "--compat", "none", "--apply"]);
    assert!(out.contains("added under Unreleased in CHANGELOG.md"), "{out}");
    assert_eq!(std::fs::read_to_string(root.join("CHANGELOG.md")).expect("cl"), "# Changelog\n\n## [Unreleased]\n\n### Fixed\n- Adding no longer panics on overflow.\n\n### Fixed\n- old\n");
    let p = policy::load(&root).expect("p");
    let _ = p;
    assert_eq!(
        write::insert("# Log\nolder\n", "### Added\n- a"),
        "# Log\n\n## [Unreleased]\n\n### Added\n- a\n\nolder\n"
    );
    assert_eq!(write::insert("", "### Added\n- a"), "## [Unreleased]\n\n### Added\n- a\n");
    assert_eq!(write::insert("## Unreleased\n", "- a"), "## Unreleased\n\n- a\n");
    assert!(write::problems(write::Kind::Commit, "", None, None)[0].contains("empty"));
    assert!(write::problems(write::Kind::Commit, &"x".repeat(80), None, None)[0].contains("80 characters"));
    assert!(write::problems(write::Kind::Commit, "a\nb", None, None)[0].contains("blank line"));
    assert!(write::problems(write::Kind::Commit, "a\n\nMISSING: the issue number", None, None)[0]
        .contains("missing"));
    t.write(".zazzle/flow.json", br#"{"checks": [{"run": "true"}], "branches": {"target": "main"}, "changelog": {"path": "CHANGELOG.md", "script": "grep -q Unreleased CHANGELOG.md"}}"#);
    let f = Fake::new(vec![("local default low", say("### Fixed\n- again"))]);
    let (out, _) = flow(&root, &f, &["write", "changelog", "--intent", "x", "--compat", "none", "--apply"]);
    assert!(out.contains("changelog check `grep -q Unreleased CHANGELOG.md`: passed"), "{out}");
    let f = Fake::new(vec![("local default low", fails("busy"))]);
    assert_eq!(flow(&root, &f, &["write", "commit", "--intent", "x"]), ("error: busy".to_string(), 2));
}

// Checks waiting for CI through a stand-in gh: pending then green, a failure with its link, a head that moves, still running at the time limit, and no checks.
#[test]
fn test_ci_wait() {
    let _g = crate::testutil::env_lock();
    let (t, root) = repo_on_feature();
    let script = |name: &str, states: &[&str]| {
        let dir = t.path().join(name);
        std::fs::create_dir_all(&dir).expect("dir");
        for (i, s) in states.iter().enumerate() {
            std::fs::write(dir.join(i.to_string()), s).expect("state");
        }
        exe(&t, &format!("{name}.sh"), &format!("n=$(cat {d}/n 2>/dev/null || echo 0)\nlast=$(ls {d} | grep -v n | sort -n | tail -1)\nf={d}/$n; [ -f $f ] || f={d}/$last\ncat $f\necho $((n+1)) > {d}/n\n", d = dir.display()))
    };
    let pr = |head: &str, checks: &str| {
        format!("{{\"number\":3,\"url\":\"https://x/pr/3\",\"headRefOid\":\"{head}\",\"isDraft\":false,\"statusCheckRollup\":[{checks}]}}")
    };
    let pending = r#"{"name":"test","status":"IN_PROGRESS","conclusion":""}"#;
    let ok = r#"{"name":"test","status":"COMPLETED","conclusion":"SUCCESS"},{"context":"lint","state":"SUCCESS"},{"name":"skip","status":"COMPLETED","conclusion":"SKIPPED"}"#;
    let bad = r#"{"name":"test","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://x/job/1"}"#;
    std::env::set_var("ZAZZLE_CI_POLL_SECS", "0.01");
    let mut results = Vec::new();
    for (name, states, limit) in [
        ("green", vec![pr("aaaaaaaaa1", pending), pr("aaaaaaaaa1", ok)], 10),
        ("red", vec![pr("aaaaaaaaa1", bad)], 10),
        ("moved", vec![pr("aaaaaaaaa1", pending), pr("bbbbbbbbb2", pending)], 10),
        ("slow", vec![pr("aaaaaaaaa1", pending)], 0),
        ("empty", vec![pr("aaaaaaaaa1", "")], 10),
    ] {
        let states: Vec<&str> = states.iter().map(String::as_str).collect();
        std::env::set_var("ZAZZLE_GH", script(name, &states));
        results.push(ci::wait(&root, if name == "red" { "3" } else { "" }, Duration::from_secs(limit)));
    }
    std::env::set_var("ZAZZLE_GH", exe(&t, "junk.sh", "echo nope\n"));
    let junk = ci::wait(&root, "", Duration::from_secs(1));
    std::env::remove_var("ZAZZLE_GH");
    std::env::remove_var("ZAZZLE_CI_POLL_SECS");
    let r: Vec<(String, bool)> = results.into_iter().map(|r| r.expect("ci")).collect();
    assert!(
        r[0].1
            && r[0].0.contains("✓ test: success")
            && r[0].0.contains("✓ lint: success")
            && r[0].0.contains("- skip: skipped")
            && r[0].0.contains("all checks passed"),
        "{}",
        r[0].0
    );
    assert!(
        !r[1].1
            && r[1].0.contains("✗ test: failure")
            && r[1].0.contains("failed: test https://x/job/1")
            && r[1].0.contains("1 check(s) failed"),
        "{}",
        r[1].0
    );
    assert!(!r[2].1 && r[2].0.starts_with("stale: the head moved from aaaaaaaa to bbbbbbbb"), "{}", r[2].0);
    assert!(
        !r[3].1 && r[3].0.contains("… test: still running") && r[3].0.contains("still running after 0 s"),
        "{}",
        r[3].0
    );
    assert!(r[4].1 && r[4].0.contains("checks: none reported"), "{}", r[4].0);
    assert!(junk.unwrap_err().starts_with("gh pr view:"));
}

// Checks how agents edit: whole files from a local model inside its scope (unsafe and outside paths refused), and a hosted agent's change outside its scope put back.
#[test]
fn test_edit() {
    let (t, root) = repo_on_feature();
    assert_eq!(
        edit::parse_files("FILE `a.rs`\n\n```rust\nfn a() {}\n```\nFILE b.rs\nno fence\nFILE \n```\nx\n```"),
        [("a.rs".to_string(), "fn a() {}\n".to_string())]
    );
    let log = Log::default();
    let local = Rung::parse("qwen-a", None).expect("rung");
    let f = Fake::new(vec![("qwen-a low", say("FILE tests/new.sh\n```\nexit 1\n```\nFILE src/lib.rs\n```\nbad\n```\nFILE ../escape\n```\nx\n```"))]);
    let far = Instant::now() + Duration::from_secs(30);
    let only_tests = |p: &str| p.starts_with("tests/");
    let e = edit::edit(&root, &f, &log, &local, "t", "p", &[], &only_tests, far).expect("local");
    assert_eq!(
        (e.changed.clone(), e.reverted.clone()),
        (vec!["tests/new.sh".to_string()], vec!["src/lib.rs".to_string(), "../escape".to_string()])
    );
    assert!(std::fs::read_to_string(root.join("src/lib.rs")).expect("lib").contains("a + b"));
    let hosted = Rung::parse("claude:x", None).expect("rung");
    let f = Fake::new(vec![(
        "claude:x medium",
        edits(&[("tests/h.sh", "exit 2\n"), ("src/lib.rs", "hosted\n")], "STATUS complete"),
    )]);
    let e = edit::edit(&root, &f, &log, &hosted, "t", "p", &[], &only_tests, far).expect("hosted");
    assert_eq!((e.changed, e.reverted), (vec!["tests/h.sh".to_string()], vec!["src/lib.rs".to_string()]));
    assert!(std::fs::read_to_string(root.join("src/lib.rs")).expect("lib").contains("a + b"), "put back");
    let _ = t;
}

// Checks test-first end to end: A's test fails for the stated reason, B's production change makes it pass, the broader checks run, and the receipts and test patch are kept.
#[test]
fn test_test_first() {
    let (t, root) = repo_on_feature();
    opt_in(&t, "");
    t.write("src/flag.txt", b"broken\n");
    g(&root, &["add", "-A"]);
    g(&root, &["commit", "-qm", "flag"]);
    let test = "grep -q fixed src/flag.txt || { echo 'expected fixed in flag'; exit 1; }\n";
    let f = Fake::new(vec![
        ("qwen-a low", say(&format!("FILE tests/flag.sh\n```\n{test}```\nSTATUS complete"))),
        (
            "claude:b medium",
            edits(&[("src/flag.txt", "fixed\n"), ("tests/flag.sh", "exit 0\n")], "STATUS complete"),
        ),
    ]);
    let (out, code) = flow(
        &root,
        &f,
        &[
            "test-first",
            "--behavior",
            "the flag reads fixed",
            "--tests",
            "tests/flag.sh",
            "--reference",
            "tests/check.sh",
            "--run",
            "sh tests/flag.sh",
            "--expect",
            "expected fixed",
            "--test-agent",
            "qwen-a",
            "--fix-agent",
            "claude:b",
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("red: `sh tests/flag.sh` failed as expected") && out.contains("test patch "),
        "{out}"
    );
    assert!(out.contains("B changed the test (tests/flag.sh); it is put back"), "{out}");
    assert!(
        out.contains("green: `sh tests/flag.sh` passes")
            && out.contains("✓ unit (test)")
            && out.contains("-- done: the test failed for the stated reason"),
        "{out}"
    );
    assert!(
        out.contains("phases (")
            && out.contains("test: local 1 call(s)")
            && out.contains("implement: hosted 1 call(s) to claude:b medium"),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("tests/flag.sh")).expect("test"),
        test,
        "the accepted assertion is unchanged"
    );
    let a = &f.prompts("qwen-a low")[0];
    assert!(a.contains("Model it on this reference test (tests/check.sh)") && a.contains("FILE path"), "{a}");
    let b = &f.prompts("claude:b medium")[0];
    assert!(b.contains("expected fixed in flag") && b.contains("read-only"), "{b}");
}

// Checks test-first refusing what proves nothing: a test that passes already, a placeholder, one that does not compile, one that fails for another reason, no test at all; and B that never makes it pass, and --no-fix.
#[test]
fn test_test_first_refusals() {
    let (t, root) = repo_on_feature();
    opt_in(&t, "");
    let run_with = |turns: Vec<(&str, Turn)>, extra: &[&str]| {
        g(&root, &["checkout", "-q", "--", "."]);
        let _ = std::fs::remove_file(root.join("tests/new.sh"));
        let f = Fake::new(turns);
        let mut args = vec![
            "test-first",
            "--behavior",
            "b",
            "--tests",
            "tests/new.sh",
            "--reference",
            "tests/check.sh",
            "--run",
            "sh tests/new.sh",
            "--expect",
            "WANTED",
            "--test-agent",
            "qwen-a",
        ];
        args.extend(extra);
        flow(&root, &f, &args)
    };
    let file = |body: &str| say(&format!("FILE tests/new.sh\n```\n{body}\n```"));
    let (out, code) = run_with(
        vec![("qwen-a low", file("exit 0")), ("qwen-a low", file("echo TODO WANTED; exit 1"))],
        &["--no-fix"],
    );
    assert_eq!(code, 1);
    assert!(
        out.contains("round 1: the test passed before any production change")
            && out.contains("round 2: the test is a placeholder")
            && out.contains("-- NOT ACCEPTED"),
        "{out}"
    );
    let (out, _) = run_with(
        vec![
            ("qwen-a low", file("echo 'error[E0425]: WANTED'; exit 1")),
            ("qwen-a low", file("echo other; exit 1")),
        ],
        &["--no-fix"],
    );
    assert!(
        out.contains("did not compile or load: error[E0425]")
            && out.contains("not for the expected reason (`WANTED` is not in its output; it ended: other)"),
        "{out}"
    );
    let (out, _) =
        run_with(vec![("qwen-a low", say("nothing")), ("qwen-a low", fails("gone"))], &["--no-fix"]);
    assert!(
        out.contains("it wrote no test inside --tests") && out.contains("A (qwen-a low) failed: gone"),
        "{out}"
    );
    let (out, code) = run_with(vec![("qwen-a low", file("echo WANTED; exit 1"))], &["--no-fix"]);
    assert_eq!(code, 0);
    assert!(out.contains("-- test accepted; no fix agent given"), "{out}");
    let (out, code) = run_with(
        vec![
            ("qwen-a low", file("echo WANTED; exit 1")),
            ("qwen-b low", say("FILE src/other.txt\n```\nx\n```")),
            ("qwen-b low", fails("tired")),
        ],
        &["--fix-agent", "qwen-b"],
    );
    assert_eq!(code, 1);
    assert!(
        out.contains("B (qwen-b low) round 1: `sh tests/new.sh` still fails")
            && out.contains("B (qwen-b low) failed: tired")
            && out.contains("-- NOT GREEN"),
        "{out}"
    );
    let (out, _) = run_with(
        vec![(
            "qwen-a low",
            say("FILE src/lib.rs\n```\nx\n```\nFILE tests/new.sh\n```\necho WANTED; exit 1\n```"),
        )],
        &["--no-fix", "--prod", "src"],
    );
    assert!(out.contains("A changed paths outside --tests, put back: src/lib.rs"), "{out}");
    let f = Fake::new(vec![]);
    assert!(flow(
        &root,
        &f,
        &[
            "test-first",
            "--behavior",
            "b",
            "--tests",
            "t",
            "--reference",
            "nope",
            "--run",
            "x",
            "--expect",
            "y"
        ]
    )
    .0
    .contains("--reference nope: no such file"));
    assert!(flow(
        &root,
        &f,
        &["test-first", "--behavior", "b", "--reference", "README.md", "--run", "x", "--expect", "y"]
    )
    .0
    .contains("--tests"));
    assert!(flow(&root, &f, &["test-first", "--behavior", "b"]).0.contains("needs --reference"));
    let _ = t;
}

// Checks viewpoint selection: the scores a change gives, the level's required viewpoints and budget, a boundary added on a feature branch, and how viewpoints group into reviewer calls.
#[test]
fn test_viewpoint_selection() {
    let diff = "diff --git a/api/auth.rs b/api/auth.rs\nnew file mode 100644\n+pub fn check_token(token: &str) -> bool { sql(token) }\n+// a comment\n";
    let paths = vec!["api/auth.rs".to_string()];
    let (scores, boundary) = review::signals(diff, &paths);
    assert_eq!(boundary, ["security", "compatibility"]);
    let score = |v: &str| scores.iter().find(|(k, _)| k == v).map(|(_, s)| *s).unwrap_or(9);
    assert_eq!((score("correctness"), score("testing"), score("cognitive-load")), (3, 3, 2));
    assert_eq!(
        review::pick("feature", &scores, &boundary),
        ["correctness", "testing", "security", "compatibility"]
    );
    let dev = review::pick("development", &scores, &[]);
    assert_eq!(dev.len(), 4);
    assert_eq!(
        review::pick("production", &scores, &[]),
        ["correctness", "compatibility", "reliability", "operations", "security"]
    );
    assert_eq!(review::pick("staging", &scores, &[]).len(), 6);
    let docs = review::signals("+# Notes\n+more words\n", &["README.md".into()]);
    assert_eq!(review::pick("feature", &docs.0, &docs.1), ["correctness", "testing"]);
    let v: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    assert_eq!(review::groups("feature", &v).len(), 1);
    assert_eq!(review::groups("development", &v).len(), 2);
    assert_eq!(review::groups("staging", &v).len(), 3);
    let f = review::parse_finding(
        "Major | `src/a.rs:3` | it overflows | panics | use wrapping_add",
        "correctness",
    )
    .expect("finding");
    assert_eq!(
        (f.severity.as_str(), f.cite.as_str(), f.fix.as_str()),
        ("major", "src/a.rs:3", "use wrapping_add")
    );
    assert_eq!(review::parse_finding("odd | a:1 | x", "v").expect("short").severity, "minor");
    assert!(review::parse_finding("too | short", "v").is_none());
}

// Checks a full review with remediation: the planner's briefs, a reviewer's verified finding, the final reviewer keeping, merging, dropping, keeping dissent and asking for a decision, the fixer's change verified and re-reviewed as resolved on the new tree, the ledger kept, and the benchmark answers.
#[test]
fn test_review_with_remediation() {
    let (t, root) = repo_on_feature();
    opt_in(&t, "");
    t.write("src/lib.rs", b"pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 0\n}\n");
    let finding = "FINDING major | src/lib.rs:2 | adding can overflow | a panic in debug builds | use wrapping_add\nFINDING minor | src/lib.rs:2 | the same overflow | panic | wrapping_add\nFINDING minor | src/nope.rs:9 | made up | x | y\nFINDING minor | src/lib.rs:1 | naming | none | rename\nFINDING minor | src/lib.rs:3 | public API changed? | clients | ask\nSCOPE src/lib.rs";
    let f = Fake::new(vec![
        ("qwen-a low", say("VIEW correctness | look at the arithmetic in src/lib.rs\nVIEW nonsense | x")),
        ("qwen-a low", say(finding)),
        ("claude:z high", say("KEEP R1 [R2] | blocker | real overflow\nDROP R3 | the file does not exist\nDISSENT R4 | the name is fine, says the final reviewer\nDECIDE R5 | may the signature change?\nnoise")),
        ("claude:z high", edits(&[("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 {\n    a.wrapping_add(b)\n}\n"), ("README.md", "sneaky\n")], "FIXED F1 wrapping_add")),
        ("qwen-a low", say("RESOLVED F1 yes | line 2 uses wrapping_add\nRESOLVED F9 yes | unknown")),
    ]);
    let (out, code) = flow(
        &root,
        &f,
        &["review", "--agents", "qwen-a,claude:z@high", "--intent", "keep adding safe", "--bench"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("review: feature/x at the feature level, base "), "{out}");
    assert!(
        out.contains("(reviewed from ")
            && out.contains("fixed in 1 round(s))")
            && out.contains("viewpoints: correctness+testing (qwen-a low)"),
        "{out}"
    );
    assert!(
        out.contains("fixed and verified on tree")
            && out.contains("F1 [correctness+testing] blocker src/lib.rs:2: adding can overflow"),
        "{out}"
    );
    assert!(out.contains("needs your decision:") && out.contains("may the signature change?"), "{out}");
    assert!(out.contains("dissent kept for you:") && out.contains("the name is fine"), "{out}");
    assert!(
        out.contains("dropped: 2 (")
            && out.contains("duplicate of R1")
            && out.contains("the file does not exist"),
        "{out}"
    );
    assert!(out.contains("gap: the fixer changed paths outside the change, put back: README.md"), "{out}");
    assert!(out.contains("benchmark:\n  1. local work:") && out.contains("  2. handoff: 3 confirmed finding(s), 1 fixed and verified before handoff, 0 left open, 1 for the user to decide") && out.contains("  3. levels: no lower level reviewed this tree"), "{out}");
    assert!(out.contains("-- review done: 1 decision(s) for you"), "{out}");
    assert_eq!(std::fs::read_to_string(root.join("README.md")).expect("readme"), "# demo\n");
    let brief = &f.prompts("qwen-a low")[1];
    assert!(
        brief.contains("look at the arithmetic in src/lib.rs") && brief.contains("✓ unit (test)"),
        "{brief}"
    );
    let again = &f.prompts("qwen-a low")[2];
    assert!(again.contains("Earlier findings still open") && again.contains("F1 ["), "{again}");
    let ledger =
        std::fs::read_to_string(repo::state(&root, "reviews").expect("state").join("feature_x.json"))
            .expect("ledger");
    assert!(
        ledger.contains("\"status\": \"fixed\"") && ledger.contains("\"status\": \"decision\""),
        "{ledger}"
    );
}

// Checks a read-only feature review that leaves its findings open, its JSON, a later review of a changed tree that carries them, a higher level's benchmark answer, and the errors.
#[test]
fn test_review_read_only_and_levels() {
    let (t, root) = repo_on_feature();
    opt_in(&t, "");
    t.write("src/lib.rs", b"pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n");
    let f = Fake::new(vec![
        ("qwen-a low", say("")),
        ("qwen-a low", say("FINDING blocker | src/lib.rs:2 | it subtracts | wrong sums | use +")),
        ("qwen-a low", fails("final reviewer is down")),
    ]);
    let (out, code) =
        flow(&root, &f, &["review", "--agents", "qwen-a", "--level", "feature", "--read-only", "--bench"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("viewpoints: correctness+testing (qwen-a low)")
            && out.contains("  open:\n    F1 [correctness+testing] blocker src/lib.rs:2: it subtracts"),
        "{out}"
    );
    assert!(
        out.contains("gap: the final reviewer (qwen-a low) failed")
            && out.contains("1 finding(s) still open (read-only review)"),
        "{out}"
    );
    let f = Fake::new(vec![
        ("qwen-a low", say("")),
        ("qwen-a low", say("SCOPE all")),
        ("qwen-a low", say("SCOPE all")),
        ("qwen-a low", say("")),
    ]);
    let (out, _) = flow(
        &root,
        &f,
        &[
            "review",
            "--agents",
            "qwen-a",
            "--level",
            "development",
            "--read-only",
            "--bench",
            "--viewpoints",
            "correctness,testing",
        ],
    );
    assert!(out.contains("3. levels: 0 new viewpoint(s) at development (), 2 repeated from lower levels on this tree (correctness, testing)"), "{out}");
    assert!(out.contains("F1 [correctness+testing] blocker"), "an open finding is carried: {out}");
    let f = Fake::new(vec![
        ("qwen-a low", say("")),
        ("qwen-a low", fails("reviewer down")),
        ("qwen-a low", say("RESOLVED F1 yes | fixed by hand")),
    ]);
    t.write("src/lib.rs", b"pub fn add(a: i32, b: i32) -> i32 {\n    a + b + 0\n}\n");
    let (out, _) = flow(
        &root,
        &f,
        &[
            "review",
            "--agents",
            "qwen-a",
            "--level",
            "development",
            "--viewpoints",
            "correctness,testing,security",
            "--json",
        ],
    );
    let v: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["level"], "development");
    assert!(v["gaps"][0].as_str().unwrap_or_default().contains("reviewer down"));
    assert!(
        v["findings"].as_array().expect("findings").iter().any(|x| x["id"] == "F1" && x["status"] == "fixed"),
        "{out}"
    );
    let f = Fake::new(vec![
        ("qwen-a low", say("")),
        ("qwen-a low", say("FINDING major | src/lib.rs:2 | x | y | z")),
        ("qwen-a low", say("KEEP R1 | major | ok")),
        ("qwen-a low", say("FILE README.md\n```\nno\n```")),
    ]);
    t.write("src/lib.rs", b"pub fn add(a: i32, b: i32) -> i32 {\n    a * b\n}\n");
    let (out, _) = flow(&root, &f, &["review", "--agents", "qwen-a", "--level", "feature"]);
    assert!(
        out.contains("gap: the fixer changed nothing for the open findings")
            || out.contains("put back: README.md"),
        "{out}"
    );
    let none = Fake::new(vec![]);
    assert!(flow(&root, &none, &["review", "--level", "qa"]).0.contains("--level is one of"));
    assert!(flow(&root, &none, &["review", "--base", "nope"]).0.contains("--base nope"));
    assert!(flow(&root, &none, &["review", "--viewpoints", "vibes", "--agents", "q"])
        .0
        .contains("unknown viewpoint `vibes`"));
    assert!(flow(&root, &none, &["review", "--rounds", "9"]).0.contains("--rounds takes 0 to 5"));
    g(&root, &["checkout", "-q", "--", "."]);
    assert!(flow(&root, &none, &["review"]).0.contains("nothing to review"));
}

// Checks the command line: help, preview, init, verify with JSON and --all, deliver through the dispatcher, unknown flags and subcommands, and the phase table.
#[test]
fn test_dispatch_preview_init() {
    let (t, root) = repo_on_feature();
    let f = Fake { installed: vec!["qwen-a".into()], ..Fake::new(vec![]) };
    assert!(flow(&root, &f, &["help"]).0.contains("zazzle flow: the repository workflow"));
    assert!(flow(&root, &f, &[]).0.contains("usage: zazzle flow"));
    assert!(flow(&root, &f, &["dance"]).0.contains("usage: zazzle flow"));
    assert!(flow(&root, &f, &["preview", "--wat", "x"]).0.contains("unknown flag --wat"));
    assert!(flow(&root, &f, &["verify", "--only"]).0.contains("--only needs a value"));
    let (out, code) = flow(&root, &f, &["preview"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("opted in: no:")
            && out.contains("branch: feature/x (feature level)")
            && out.contains("pull requests target: main")
            && out.contains("checks:\n  none found")
            && out.contains("local models: qwen-a")
            && out.contains("-- preview only"),
        "{out}"
    );
    let (out, _) = flow(&root, &f, &["init"]);
    assert!(out.contains("\"target\": \"main\"") && out.contains("-- not saved"), "{out}");
    t.write("Makefile", b"test:\n\ttrue\n");
    t.write("AGENTS.md", b"PRs target `dev`.\n");
    t.write("docs/x.md", b"Open PRs into `staging`.\n");
    let (out, _) = flow(&root, &f, &["init", "--write"]);
    assert!(
        out.contains("conflict to settle in the file") && out.contains("-- saved .zazzle/flow.json"),
        "{out}"
    );
    assert!(flow(&root, &f, &["init", "--write"]).0.contains("already exists"));
    let (out, _) = flow(&root, &f, &["preview", "--base", "main"]);
    assert!(
        out.contains("opted in: yes")
            && out.contains("+ make (test): `make test` in .")
            && out.contains("conflict: ")
            && out.contains("configured agents: "),
        "{out}"
    );
    let (out, code) = flow(&root, &f, &["verify", "--json"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(serde_json::from_str::<Value>(&out).expect("json")["green"], true);
    assert_eq!(flow(&root, &f, &["verify", "--all"]).1, 0);
    assert_eq!(
        flow(&root, &f, &["verify", "--base", "main", "--timeout", "x"]).0,
        "error: --timeout takes a number of seconds, not `x`"
    );
    t.write("src/lib.rs", b"// changed\n");
    let (out, code) = flow(&root, &f, &["deliver", "src", "-m", "Change lib"]);
    assert_eq!(code, 0, "{out}");
    let (out, code) = flow(&root, &f, &["ci", "--pr", "1", "--timeout", "1"]);
    assert_eq!(code, 2, "no gh: {out}");
    let calls = vec![
        calls::Call {
            role: "review x".into(),
            rung: "claude:a high".into(),
            hosted: true,
            sent: vec![("diff".into(), 100), ("brief".into(), 20)],
            input: 300,
            cached: 100,
            tokens: 40,
            rounds: 2,
            cost: Some(0.1),
            secs: 3.0,
            ok: true,
            ..Default::default()
        },
        calls::Call {
            role: "briefing".into(),
            rung: "qwen low".into(),
            tokens: 10,
            queued: 1.5,
            secs: 4.0,
            ok: true,
            ..Default::default()
        },
    ];
    let table = calls::phases(&calls, 7.0);
    assert!(table.contains("review: hosted 1 call(s) to claude:a high — sent ~120 tokens (diff 100, brief 20); reported 300 in, 100 cached, 40 out, 2 rounds, $0.1000") && table.contains("briefing: local 1 call(s), 1.5 s queued, 4.0 s in all, 10 out"), "{table}");
}

// Checks the less common shapes of tool output, discovery and guidance: dotnet and go variants, a failing test counted with exit 0, a check named with spaces, Python markers, deep folders, quoted branch names said twice, and an origin HEAD.
#[test]
fn test_parse_and_discover_variants() {
    assert_eq!(verify::parse("Passed!  - Failed:     0, Passed:     4, Skipped:     1").0, 4);
    assert_eq!(verify::parse("==== 2 failed in 0.3s ====").1, 2);
    assert_eq!(
        verify::parse("ok \tpkg/a\nfailed tests/x.py::test_y\nFAILED x").4,
        ["failed tests/x.py::test_y", "FAILED x"]
    );
    assert_eq!(verify::parse("Failed! but no counts").0, 0);
    let (t, root) = repo_on_feature();
    let c = policy::Check {
        name: "my check".into(),
        run: "/bin/sh -c 'echo \"test result: FAILED. 0 passed; 1 failed; 0 ignored\"'".into(),
        dir: ".".into(),
        kind: "test".into(),
        paths: Vec::new(),
    };
    let o = verify::run_check(&root, &c, &root.join("x.log"), Duration::from_secs(10));
    assert_eq!((o.status.as_str(), o.failed), ("failed", 1), "exit 0 with a failed test is a failure");
    let mut p = policy::load(&root).expect("p");
    p.checks = vec![c];
    let r = verify::verify(&root, &p, None, &[], Duration::from_secs(10)).expect("verify");
    assert!(r.checks[0].log.ends_with("-my-check.log"), "{}", r.checks[0].log);
    let o = verify::Outcome { status: "passed".into(), secs: 1.0, ..verify::Outcome::default() };
    assert_eq!(o.summary(), "passed 1.0 s");
    let d = TempDir::new();
    d.write("a/pytest.ini", b"");
    d.write("b/setup.cfg", b"");
    d.write("c/d/e/f/go.mod", b"");
    d.write("c/d/e/go.mod", b"");
    let found: Vec<String> = policy::discover(d.path()).into_iter().map(|c| c.name).collect();
    assert_eq!(found, ["pytest a", "pytest b", "go c/d/e"], "four folders down is not searched");
    t.write(
        "AGENTS.md",
        b"A pr line without a branch.\nPRs target 'dev' here.\nThe PR goes into `dev` too.\n",
    );
    t.write("docs/a/b/c/d/deep.md", b"PRs target `staging`.\n");
    t.write("docs/notes.txt", b"PRs target `staging`.\n");
    let s = policy::statements(&root);
    assert!(s.iter().all(|(_, b)| b == "dev" || b == "main"), "{s:?}");
    let p = policy::load(&root).expect("p");
    assert_eq!(p.target, "dev", "the same branch said twice is one statement: {:?}", p.conflicts);
    let bare = TempDir::new();
    g(bare.path(), &["init", "-q", "--bare", "-b", "trunk"]);
    g(&root, &["remote", "add", "origin", &bare.path().display().to_string()]);
    g(&root, &["push", "-q", "origin", "main:trunk"]);
    let clone = TempDir::new();
    g(clone.path(), &["clone", "-q", &bare.path().display().to_string(), "c"]);
    assert_eq!(
        policy::load(&clone.path().join("c")).expect("clone").target,
        "trunk",
        "origin's HEAD names the default branch"
    );
    let fresh = TempDir::new();
    g(fresh.path(), &["init", "-q"]);
    fresh.write("a.txt", b"a");
    assert!(repo::worktree_tree(fresh.path()).is_ok(), "a repository without an index yet");
    assert!(repo::under("x", &["".into()]));
}

// Checks review edges: a repository without checks, a clean review, a hosted reviewer, malformed and out-of-range adjudication, a finding kept with a bad severity, an unresolved re-review, a fixer that stays in scope while the checks stay red, the round limit, and a benchmark run on hosted agents only.
#[test]
fn test_review_edges() {
    let (t, root) = repo_on_feature();
    g(&root, &["checkout", "-qb", "fix-1"]);
    t.write(
        "src/lib.rs",
        b"pub fn add(a: i32, b: i32) -> i32 {\n    // why: keep it simple\n    a + b + 0\n}\n",
    );
    let f = Fake::new(vec![("qwen-a low", say("")), ("qwen-a low", say("SCOPE src/lib.rs\nFINDING bad"))]);
    let (out, code) = flow(&root, &f, &["review", "--agents", "qwen-a", "--read-only"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("verification: no checks configured or found")
            && out.contains("  no confirmed findings")
            && out.contains("-- review done: nothing left for you"),
        "{out}"
    );
    opt_in(&t, "");
    g(&root, &["checkout", "-q", "fix-1"]);
    g(&root, &["merge", "-q", "main"]);
    let big = format!(
        "pub fn add(a: i32, b: i32) -> i32 {{\n    a + b + 0\n}}\n{}",
        "// filler line that makes the diff long enough to be cut\n".repeat(3000)
    );
    t.write("src/lib.rs", big.as_bytes());
    let f = Fake::new(vec![
        ("qwen-a low", say("VIEW correctness | arithmetic")),
        ("claude:r medium", say("FINDING huge | src/lib.rs:2 | wrong | bad | fix it\nFINDING minor | src/lib.rs:1 | x | y | z\nRESOLVED F1")),
        ("qwen-a low", say("KEEP R1 | huge | kept\nKEEP R99 | minor | out of range\nNOTE R2 | ignored word")),
        ("claude:fx medium", edits(&[("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 {\n    a.wrapping_add(b)\n}\n"), ("tests/check.sh", "exit 1\n")], "FIXED F1")),
        ("claude:r medium", say("RESOLVED F1 no | still wrong\nRESOLVED F2 yes | gone")),
    ]);
    let (out, code) = flow(
        &root,
        &f,
        &[
            "review",
            "--level",
            "staging",
            "--viewpoints",
            "correctness",
            "--reviewers",
            "claude:r",
            "--final",
            "qwen-a",
            "--fix-agent",
            "claude:fx",
            "--agents",
            "qwen-a",
            "--rounds",
            "1",
            "--bench",
        ],
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.starts_with("review: fix-1 at the staging level"), "{out}");
    assert!(
        out.contains("F1 [correctness] minor src/lib.rs:2: wrong") && out.contains("  open:"),
        "the bad severity stays as parsed: {out}"
    );
    assert!(
        out.contains("fixed, but the checks are not green:") && out.contains("F2 [correctness]"),
        "{out}"
    );
    assert!(
        out.contains("1. local work: 2 local call(s) (briefing, adjudicate)")
            && out.contains("extra hosted round(s)"),
        "{out}"
    );
    assert!(out.contains("-- review done, checks not green: 2 finding(s) still open"), "{out}");
    let sent = &f.prompts("claude:r medium")[0];
    assert!(
        sent.contains("You may read any file") && sent.contains("[cut here; read the files for the rest]"),
        "the diff is capped for reviewers"
    );
    let ledger = repo::state(&root, "reviews").expect("state").join("fix-1.json");
    assert!(ledger.is_file());
    let f = Fake::new(vec![
        ("claude:p medium", say("")),
        ("claude:p medium", say("FINDING major | src/lib.rs:2 | x | y | z")),
        ("claude:p medium", say("KEEP R1 | major | ok")),
    ]);
    let (out, _) = flow(
        &root,
        &f,
        &["review", "--agents", "claude:p", "--level", "feature", "--rounds", "0", "--bench"],
    );
    assert!(out.contains("1. local work: 0 local call(s) (none)"), "{out}");
    assert!(
        out.contains("3. levels: 0 new viewpoint(s) at feature") || out.contains("3. levels: no lower level"),
        "{out}"
    );
}

// Checks the dispatcher's remaining paths: a red verify, push without a pull request, a green CI wait, reviewers from the policy, a commit draft saved, a protected branch and the hosted agents found in preview.
#[test]
fn test_dispatch_more() {
    let _g = crate::testutil::env_lock();
    let (t, root) = repo_on_feature();
    opt_in(&t, ", \"agents\": {\"ladder\": [\"qwen-l\"], \"reviewers\": [\"qwen-r\"]}");
    t.write("tests/check.sh", b"echo nope; exit 1\n");
    let f = Fake::new(vec![]);
    assert_eq!(flow(&root, &f, &["verify"]).1, 1);
    t.write("tests/check.sh", b"exit 0\n");
    let f = Fake::new(vec![("qwen-l low", say("Tidy the adder"))]);
    let t2 = t.write("src/lib.rs", b"// tidy\n");
    let _ = t2;
    let (out, code) = flow(&root, &f, &["write", "commit", "--intent", "tidy", "--apply"]);
    assert!(code == 0 && out.contains("commit-message.txt"), "{out}");
    g(&root, &["add", "src/lib.rs"]);
    let (out, _) = flow(
        &root,
        &Fake::new(vec![("qwen-l low", say("Staged change"))]),
        &["write", "commit", "--intent", "x"],
    );
    assert_eq!(out, "Staged change");
    let bare = TempDir::new();
    g(bare.path(), &["init", "-q", "--bare"]);
    g(&root, &["remote", "add", "origin", &bare.path().display().to_string()]);
    let (out, code) = flow(&root, &f, &["deliver", "--paths", "src", "-m", "Tidy", "--push"]);
    assert!(code == 0 && out.contains("push: pushed to origin/feature/x") && !out.contains("pr:"), "{out}");
    let gh = exe(&t, "gh", "echo '{\"number\":1,\"url\":\"u\",\"headRefOid\":\"abcdefabcd\",\"isDraft\":true,\"statusCheckRollup\":[{\"name\":\"t\",\"status\":\"COMPLETED\",\"conclusion\":\"SUCCESS\"}]}'\n");
    std::env::set_var("ZAZZLE_GH", &gh);
    let ci = flow(&root, &f, &["ci"]);
    let pr_gh = exe(&t, "gh2", "case \"$2\" in list) echo '';; create) echo 'https://x/pr/2';; esac\n");
    std::env::set_var("ZAZZLE_GH", &pr_gh);
    t.write("src/lib.rs", b"// tidy 2\n");
    let pr = flow(&root, &f, &["deliver", "--paths", "src", "-m", "Tidy 2", "--pr"]);
    let claude = exe(&t, "claude", "exit 0\n");
    std::env::set_var("ZAZZLE_CLAUDE", &claude);
    let preview = flow(&root, &Fake::new(vec![]), &["preview"]);
    std::env::remove_var("ZAZZLE_CLAUDE");
    std::env::remove_var("ZAZZLE_GH");
    assert!(ci.1 == 0 && ci.0.contains("(draft)") && ci.0.contains("all checks passed"), "{}", ci.0);
    assert!(pr.0.contains("pr: https://x/pr/2 (opened)"), "{}", pr.0);
    assert!(
        preview.0.contains(&format!("hosted agents found: {}", claude.display()))
            && preview.0.contains("local models: none installed"),
        "{}",
        preview.0
    );
    let f = Fake::new(vec![("qwen-l low", say("")), ("qwen-r low", say("SCOPE all"))]);
    let (out, _) = flow(&root, &f, &["review", "--into", "main", "--level", "feature", "--read-only"]);
    assert!(out.contains("(qwen-r low)"), "reviewers come from the policy: {out}");
    g(&root, &["checkout", "-q", "main"]);
    let (out, _) = flow(&root, &Fake::new(vec![]), &["preview"]);
    assert!(out.contains("(production level, protected: deliver refuses it)"), "{out}");
}

// Checks test-first with hosted agents and production paths: the hosted author's test, a fixer that also touches a path outside --prod, an exit-0 failure accepted as red, a command that is not installed, and broader checks that end red.
#[test]
fn test_test_first_hosted() {
    let (t, root) = repo_on_feature();
    opt_in(&t, "");
    t.write("tests/check.sh", b"[ ! -f src/flag.txt ]\n");
    g(&root, &["commit", "-qam", "a check the fix breaks"]);
    let counted = "if grep -q fixed src/flag.txt; then echo 'test result: ok. 1 passed; 0 failed; 0 ignored'; else echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored'; echo WANTED; fi\n";
    let f = Fake::new(vec![
        ("claude:a medium", edits(&[("tests/new.sh", counted)], "STATUS complete")),
        (
            "claude:b medium",
            edits(
                &[("src/flag.txt", "fixed\n"), ("README.md", "x\n"), ("tests/check.sh", "exit 1\n")],
                "STATUS complete",
            ),
        ),
    ]);
    let (out, code) = flow(
        &root,
        &f,
        &[
            "test-first",
            "--behavior",
            "b",
            "--tests",
            "tests/new.sh",
            "--reference",
            "tests/check.sh",
            "--run",
            "sh tests/new.sh",
            "--expect",
            "WANTED",
            "--test-agent",
            "claude:a",
            "--fix-agent",
            "claude:b",
            "--prod",
            "src",
        ],
    );
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("red: `sh tests/new.sh` failed as expected"),
        "an exit-0 run with a failed test is red: {out}"
    );
    assert!(out.contains("B changed paths outside its scope, put back: README.md, tests/check.sh"), "{out}");
    assert!(
        out.contains("green: `sh tests/new.sh` passes")
            && out.contains("-- done, but the broader checks are not green"),
        "{out}"
    );
    assert!(
        f.prompts("claude:a medium")[0].contains("Reply with the STATUS and CONFIDENCE lines when done.")
            && f.prompts("claude:b medium")[0].contains("You may change: src.")
    );
    let f = Fake::new(vec![
        ("qwen-a low", say("FILE tests/z.sh\n```\nexit 1\n```")),
        ("qwen-a low", say("FILE tests/z.sh\n```\nexit 1\n```")),
    ]);
    let (out, _) = flow(
        &root,
        &f,
        &[
            "test-first",
            "--behavior",
            "b",
            "--tests",
            "tests/z.sh",
            "--reference",
            "tests/check.sh",
            "--run",
            "no-such-runner-zz tests/z.sh",
            "--expect",
            "x",
            "--test-agent",
            "qwen-a",
            "--no-fix",
        ],
    );
    assert!(out.contains("the command could not run: `no-such-runner-zz` is not installed"), "{out}");
    assert_eq!(
        testfirst::filter_patch(
            "diff --git a/src/a b/src/a\n+x\ndiff --git a/tests/t b/tests/t\n+y\n",
            &["tests".into()]
        ),
        "diff --git a/tests/t b/tests/t\n+y\n"
    );
}

// Checks the writer's remaining cases: a changelog with no sections of its own, a long diff cut, and drafts in a bare or unclosed fence.
#[test]
fn test_write_more() {
    let (t, root) = repo_on_feature();
    opt_in(&t, "");
    t.write("CHANGELOG.md", b"# Changelog\n");
    g(&root, &["add", "-A"]);
    g(&root, &["commit", "-qm", "cl"]);
    t.write(
        "src/lib.rs",
        "// line that is long enough to count for something in the token estimate\n".repeat(2000).as_bytes(),
    );
    let f = Fake::new(vec![("local default low", say("```\n### Security\n- tighter\n```"))]);
    let (out, code) = flow(&root, &f, &["write", "changelog", "--intent", "x", "--compat", "none"]);
    assert_eq!((out.as_str(), code), ("### Security\n- tighter", 0));
    let sent = &f.prompts("local default low")[0];
    assert!(
        sent.contains("[diff cut here]")
            && sent.contains("Added, Changed, Deprecated, Removed, Fixed, Security"),
        "default sections and a cut diff"
    );
    for draft in ["```", "```oops"] {
        let f = Fake::new(vec![("local default low", say(draft))]);
        assert_eq!(
            flow(&root, &f, &["write", "commit", "--intent", "x"]).0,
            draft,
            "a bare or unclosed fence is kept as written"
        );
    }
}
