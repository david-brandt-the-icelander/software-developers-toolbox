// Tests for brief: sections inside purpose ranges, mandatory rules without inference, the model's ranking, the shared ledger and the budget cut at section boundaries.
use super::*;
use crate::testutil::TempDir;
use ai_agent_rules::list_rules;
use std::cell::RefCell;

// A rule file with a preamble, two headings, a fenced heading-like line and a purpose segment, and two skills.
fn repo(t: &TempDir) -> PathBuf {
    let root = t.path().canonicalize().expect("root");
    std::fs::create_dir_all(root.join(".git")).expect("git");
    t.write(
        "AGENTS.md",
        b"# Repo\nIntro line.\n## Build\ncargo build\n```\n# not a heading\n```\n## Release\nTag it.\n<!-- #zazzle-purpose-begin:review -->\n## Review\nCheck the contract.\n<!-- #zazzle-purpose-end:review -->\n",
    );
    t.write(
        "agents/skills/deploy/SKILL.md",
        b"---\nname: deploy\ndescription: Deploy the site to production\n---\n# Deploy\nRun deploy.sh.\n",
    );
    t.write(
        "agents/skills/paint/SKILL.md",
        b"---\nname: paint\ndescription: Paint the logo\n---\n# Paint\nUse the brush.\n",
    );
    root
}

// A ledger backed by a list, as a session would be.
fn keep(seen: &RefCell<Vec<String>>) -> (impl Fn(&str) -> bool + '_, impl Fn(&str) + '_) {
    (
        move |k: &str| seen.borrow().iter().any(|s| s == k),
        move |k: &str| seen.borrow_mut().push(k.to_string()),
    )
}

// Runs brief at the root with a ledger and a scripted model; returns the output and how many times the model was asked.
fn go(
    root: &Path,
    argv: &[&str],
    l: Option<&Ledger>,
    purposes: &[&str],
    answer: Result<&str, &str>,
) -> (Result<String, String>, usize) {
    let asked = RefCell::new(0);
    let ask = |i: &str, menu: &str, g: &str| {
        *asked.borrow_mut() += 1;
        assert!(i.contains("rules that apply are already given") && g.starts_with("root ::= \"1:\" v"));
        assert!(!menu.contains("§"), "rules are never put to the model: {menu}");
        answer.map(str::to_string).map_err(str::to_string)
    };
    let env = Env {
        cwd: root,
        home: None,
        purposes: purposes.iter().map(|s| s.to_string()).collect(),
        ledger: l,
        budget: DEFAULT_BUDGET,
        ask: &ask,
    };
    let out = run(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>(), env);
    let n = *asked.borrow();
    (out, n)
}

// Checks rules split at headings outside fences and without purpose markers, ranges inside a purpose segment, and skills whole.
#[test]
fn test_brief_sections() {
    let t = TempDir::new();
    let root = repo(&t);
    let all = list_rules(&root, None, &[], Path::new("/")).expect("list");
    let s = sections(&all);
    let heads: Vec<(&str, (usize, usize))> = s.iter().map(|x| (x.heading.as_str(), x.lines)).collect();
    assert_eq!(heads[..4], [("Repo", (1, 2)), ("Build", (3, 7)), ("Release", (8, 9)), ("Review", (11, 12))]);
    assert!(s[1].text.contains("# not a heading") && !s[3].text.contains("zazzle-purpose"));
    assert!(s[4].menu.contains("Deploy the site") && !s[4].mandatory());
    let review = list_rules(&root, None, &["review".into()], Path::new("/")).expect("review");
    let r = sections(&review);
    assert_eq!((r[0].heading.as_str(), r[0].lines, r[0].within), ("Review", (11, 12), Some((11, 12))));
    assert_eq!(verdicts(2), "root ::= \"1:\" v \" \" \"2:\" v\nv ::= \"h\" | \"m\" | \"n\"");
    assert_eq!(ranked("1:m 2:h 2:n 3:y 9:h 99999999999999999999999:h", 3), vec![1, 2, 0]);
    let unreadable = Found { kind: Kind::Rule, path: root.join("x.md"), lines: Vec::new(), text: None };
    assert!(sections(&[unreadable]).is_empty());
}

// Checks every rule section is sent whatever the model says, the model ranks only the skills, and a failing model still yields the rules.
#[test]
fn test_brief_mandatory_rules_and_ranking() {
    let t = TempDir::new();
    let root = repo(&t);
    let (out, n) = go(&root, &["ship", "it", "--path", ".", "--path", "AGENTS.md"], None, &[], Ok("1:h 2:n"));
    let out = out.expect("brief");
    assert_eq!(n, 1);
    assert!(
        out.starts_with(
            "brief for: ship it\nscope: ., AGENTS.md\n4 rule sections that apply (always sent); 1 of 2 skills"
        ),
        "{out}"
    );
    assert!(out.contains("==> rule AGENTS.md:8-9 <==\n## Release\nTag it.\n\n"), "{out}");
    assert!(out.contains("==> skill agents/skills/deploy/SKILL.md <==\n---\nname: deploy"), "{out}");
    assert!(out.ends_with("-- not ranked as needed (zazzle get-rules PATH --skill-bodies sends them): skill agents/skills/paint/SKILL.md"), "{out}");
    let (down, _) = go(&root, &["x"], None, &[], Err("local model: off"));
    let down = down.expect("still a brief");
    assert!(
        down.contains("skills not ranked: local model: off\n4 rule sections") && down.contains("## Build"),
        "{down}"
    );
}

// Checks explicit purposes and paths narrow what applies before inference: no skills to rank means no model call.
#[test]
fn test_brief_purposes_and_paths_before_inference() {
    let t = TempDir::new();
    let root = repo(&t);
    t.write("api/AGENTS.md", b"# API\nNever change the published contract.\n");
    t.write("api/a.cs", b"class A {}\n");
    std::fs::remove_dir_all(root.join("agents")).expect("no skills");
    let (out, n) =
        go(&root, &["review", "the", "api", "--path", "api/a.cs", "--purpose", "review"], None, &[], Ok(""));
    let out = out.expect("brief");
    assert_eq!(n, 0, "nothing optional, so the model is not asked");
    assert!(out.contains("scope: api/a.cs; purposes: review\n2 rule sections"), "{out}");
    assert!(out.contains("## Review\nCheck the contract.") && !out.contains("## Build"), "{out}");
    assert!(out.contains("==> rule api/AGENTS.md:1-2 <==\n# API\nNever change"), "{out}");
    let (session, _) = go(&root, &["review"], None, &["review"], Ok(""));
    assert!(session.expect("session").contains("purposes: review"), "the session's purposes apply");
    assert_eq!(purposes(&["a=b".into(), "--purpose=review".into()]), Some(vec!["review".to_string()]));
    assert!(go(&root, &["x", "--purpose", "-bad"], None, &[], Ok(""))
        .0
        .expect_err("bad")
        .contains("not a purpose"));
}

// Checks the shared ledger: a second brief repeats nothing, get-rules' purpose range counts as delivered, and only what was sent is recorded.
#[test]
fn test_brief_shared_ledger() {
    let t = TempDir::new();
    let root = repo(&t);
    let seen = RefCell::new(Vec::new());
    let (has, mark) = keep(&seen);
    let l = Ledger { has: &has, mark: &mark };
    let (first, _) = go(&root, &["ship"], Some(&l), &[], Ok("1:h 2:n"));
    assert!(first.expect("first").contains("## Release"));
    let canon = canonical(&root.join("AGENTS.md"));
    let text = std::fs::read_to_string(&canon).expect("text");
    assert!(
        seen.borrow().contains(&file_key(&canon, &text)),
        "every section of the file was sent, so the file is marked"
    );
    let paint = canonical(&root.join("agents/skills/paint/SKILL.md"));
    let paint_key = file_key(&paint, &std::fs::read_to_string(&paint).expect("paint"));
    assert!(!seen.borrow().contains(&paint_key), "a skill left out is not recorded");
    let (second, n) = go(&root, &["ship"], Some(&l), &[], Ok("1:h"));
    let second = second.expect("second");
    assert_eq!(n, 1, "only the paint skill is left to rank");
    assert!(
        second.contains("0 rule sections")
            && second
                .contains("-- already delivered this session, still in force: rule AGENTS.md:1-2 § Repo;"),
        "{second}"
    );
    assert!(second.contains("==> skill agents/skills/paint/SKILL.md <=="), "{second}");
    let other = RefCell::new(vec![range_key(&canon, &text, (11, 12))]);
    let (has2, mark2) = keep(&other);
    let l2 = Ledger { has: &has2, mark: &mark2 };
    let (r, _) = go(&root, &["review"], Some(&l2), &["review"], Ok("1:n 2:n"));
    assert!(r.expect("review").contains("0 rule sections"), "get-rules' range covers the section");
}

// Checks the budget: rules are always sent whole, skills that do not fit are left out whole and named, never cut mid-section.
#[test]
fn test_brief_budget_cuts_between_sections() {
    let t = TempDir::new();
    let root = repo(&t);
    let (out, _) = go(&root, &["ship", "--budget", "60"], None, &[], Ok("1:h 2:h"));
    let out = out.expect("brief");
    assert!(out.contains("## Release\nTag it.") && out.contains("Check the contract."), "{out}");
    assert!(!out.contains("Run deploy.sh") && !out.contains("Use the brush"), "{out}");
    assert!(out.contains("-- needed but over the budget, left out whole (raise --budget or read them): skill agents/skills/deploy/SKILL.md; skill agents/skills/paint/SKILL.md"), "{out}");
    let (tiny, _) = go(&root, &["ship", "--budget=5"], None, &[], Ok("1:n 2:n"));
    assert!(tiny.expect("tiny").contains("over the budget of 5; they are sent whole"));
    assert!(go(&root, &["x", "--budget", "0"], None, &[], Ok("")).0.is_err());
}

// Checks the command line, an empty folder and the MCP arguments.
#[test]
fn test_brief_args() {
    let t = TempDir::new();
    let root = t.path().canonicalize().expect("root");
    std::fs::create_dir_all(root.join(".git")).expect("git");
    assert_eq!(go(&root, &[], None, &[], Ok("")).0, Err(USAGE.into()));
    assert!(go(&root, &["x", "--path"], None, &[], Ok(""))
        .0
        .expect_err("value")
        .contains("--path needs a value"));
    assert!(go(&root, &["x"], None, &[], Ok(""))
        .0
        .expect("none")
        .ends_with("no rule or skill files apply here"));
    let v = args(&serde_json::json!({"intent": "go", "paths": ["a"], "purpose": ["review"], "budget": 900}))
        .expect("args");
    assert_eq!(v, ["brief", "go", "--path", "a", "--purpose", "review", "--budget", "900"]);
}
