// Tests for investigate with a scripted local model: lookups, scope, citations, rules kept apart, and the ways a run stops.
use super::*;
use crate::testutil::TempDir;
use std::cell::RefCell;

// A local model that replies from a script, one reply per turn, and records the system messages and transcripts it was given.
struct Script {
    replies: RefCell<Vec<Result<Reply, String>>>,
    seen: RefCell<Vec<(String, String)>>,
}

impl Script {
    // Builds a script from replies (Err for a failing turn).
    fn new(replies: Vec<Result<&str, &str>>) -> Script {
        let replies = replies
            .into_iter()
            .rev()
            .map(|r| r.map(|t| Reply { text: t.into(), truncated: t.contains("CUT") }).map_err(String::from))
            .collect();
        Script { replies: RefCell::new(replies), seen: RefCell::new(Vec::new()) }
    }
}

impl Local for Script {
    // Gives the next scripted reply.
    fn ask(&self, system: &str, transcript: &str) -> Result<Reply, String> {
        self.seen.borrow_mut().push((system.into(), transcript.into()));
        self.replies.borrow_mut().pop().unwrap_or_else(|| Ok(Reply { text: String::new(), truncated: false }))
    }
}

// Builds a repository with an API and a simulator that both define Handle, and a rule file in the API folder.
fn repo() -> (TempDir, PathBuf) {
    let t = TempDir::new();
    let root = t.path().canonicalize().expect("root");
    std::fs::create_dir_all(root.join(".git")).expect("git");
    t.write("api/AGENTS.md", b"Never change the published contract.\n");
    t.write(
        "api/Endpoint.cs",
        b"class Endpoint {\n  void Handle(Request r) {\n    if (r.Key == null) throw new Missing();\n    Store(r);\n  }\n}\n",
    );
    t.write("sim/Fake.cs", b"class Fake {\n  void Handle(Request r) { }\n}\n");
    (t, root)
}

// Runs investigate in a session with the script.
fn go(
    root: &std::path::Path,
    e: &crate::engine::Engine,
    s: &Script,
    argv: &[&str],
) -> Result<Output, String> {
    let ctx = Ctx {
        engine: e,
        cwd: root.to_path_buf(),
        session: "s1".into(),
        roots: Vec::new(),
        stdin: RefCell::new(None),
        caller: RefCell::new(None),
    };
    investigate(&ctx, &argv.iter().map(|a| a.to_string()).collect::<Vec<_>>(), s)
}

// Checks a full run: the starting symbol's refs stay inside the scope, a lookup outside it is refused, a cited finding comes back with its literal lines, a bad citation is rejected, and the rules reach the model apart from the evidence and the caller as notes.
#[test]
fn test_investigate_cited_findings() {
    let (_t, root) = repo();
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![
        Ok("READ api/Endpoint.cs\nREAD sim/Fake.cs"),
        Ok("ANSWER\nFINDING api/Endpoint.cs:2-3 Handle rejects a request without a key\nFINDING api/Endpoint.cs:99 made up\nUNKNOWN where Store writes"),
    ]);
    let o =
        go(&root, &e, &s, &["Where", "is", "the", "key", "checked?", "--path", "api", "--symbol", "Handle"])
            .expect("run");
    let text = &o.text;
    assert!(
        text.starts_with("investigate: Where is the key checked?\nscope: api\nfindings (1 verified):\n"),
        "{text}"
    );
    assert!(text.contains("1. Handle rejects a request without a key\n   api/Endpoint.cs:2-3\n   2\t  void Handle(Request r) {\n   3\t    if (r.Key == null) throw new Missing();\n"), "{text}");
    assert!(
        text.contains("rejected citations:\n- api/Endpoint.cs:99 made up: line 99 is past the end (6 lines)"),
        "{text}"
    );
    assert!(text.contains("unknowns:\n- where Store writes\n"), "{text}");
    assert!(text.ends_with("-- local model, 2 of 6 turns, 3 lookups (REFS Handle; READ api/Endpoint.cs; READ sim/Fake.cs); complete. Excerpts are read from the files; claims are the model's."), "{text}");
    let seen = s.seen.borrow();
    let (first_system, first) = &seen[0];
    assert!(
        first.contains("### REFS Handle")
            && first.contains("api/Endpoint.cs")
            && !first.contains("sim/Fake.cs"),
        "{first}"
    );
    assert!(!first_system.contains("published contract"), "no rules before a read");
    let (system, second) = &seen[1];
    assert!(
        second.contains("refused: sim/Fake.cs is not an existing path inside the scope (api)"),
        "{second}"
    );
    assert!(
        system.contains("Rules that apply to these files")
            && system.contains("Never change the published contract."),
        "{system}"
    );
    assert!(!second.contains("published contract"), "rules are not evidence: {second}");
    assert!(
        o.notes.contains("[instructions for this path: api/AGENTS.md]\nNever change the published contract."),
        "{}",
        o.notes
    );
    let again = Script::new(vec![Ok("ANSWER\nFINDING api/Endpoint.cs:4 stores it")]);
    let o2 = go(&root, &e, &again, &["q", "--path", "api"]).expect("again");
    assert!(o2.notes.is_empty(), "the caller already has these rules: {}", o2.notes);
    assert!(o2.text.contains("api/Endpoint.cs:4 (not among the lookups)"), "{}", o2.text);
}

// Checks the ways a run stops early, each reported as INCOMPLETE, and that a failure before anything was seen is an error.
#[test]
fn test_investigate_stops() {
    let (_t, root) = repo();
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![Ok("I think it is fine"), Ok("Still\nfine")]);
    let o = go(&root, &e, &s, &["q"]).expect("noise");
    assert!(o.text.contains("findings (0 verified):\n(none)\n") && o.text.contains("INCOMPLETE: it replied twice with neither lookups nor an ANSWER (the last began: \"Still fine\")"), "{}", o.text);
    assert!(s.seen.borrow()[1].1.contains(
        "### NOTE\nYour last reply had neither lookups nor ANSWER (it began: \"I think it is fine\")"
    ));
    let s = Script::new(vec![Ok("hmm"), Ok("ANSWER\nUNKNOWN all of it")]);
    let o = go(&root, &e, &s, &["q"]).expect("recovers");
    assert!(o.text.contains("- all of it") && o.text.contains("complete."), "{}", o.text);
    assert_eq!(began(""), "an empty reply");
    assert!(began(&"x".repeat(200)).ends_with("…\""));
    let o = go(
        &root,
        &e,
        &Script::new(vec![Ok("FIND Store"), Ok("READ api/Endpoint.cs")]),
        &["q", "--steps", "2"],
    )
    .expect("out");
    assert!(o.text.contains("INCOMPLETE: it gave no ANSWER when its lookups ran out (its reply began: \"READ api/Endpoint.cs\")"), "{}", o.text);
    let o =
        go(&root, &e, &Script::new(vec![Ok("OUTLINE api/Endpoint.cs"), Err("local model: timeout")]), &["q"])
            .expect("fail");
    assert!(
        o.text.contains("the investigation stopped early: the local model failed: local model: timeout"),
        "{}",
        o.text
    );
    assert_eq!(
        go(&root, &e, &Script::new(vec![Err("local model: no model")]), &["q"]).err().as_deref(),
        Some("local model: no model")
    );
    let o = go(&root, &e, &Script::new(vec![Ok("ANSWER CUT\nUNKNOWN everything")]), &["q"]).expect("cut");
    assert!(o.text.contains("INCOMPLETE: a reply was cut short by the token limit"), "{}", o.text);
}

// Checks the command line: the question, scope, symbols and steps, and its errors.
#[test]
fn test_investigate_args() {
    let (_t, root) = repo();
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![]);
    assert_eq!(go(&root, &e, &s, &[]).err().as_deref(), Some(USAGE));
    assert!(go(&root, &e, &s, &["q", "--steps", "99"]).expect_err("steps").contains("--steps takes 1 to 12"));
    assert!(go(&root, &e, &s, &["q", "--path"]).expect_err("value").contains("--path needs a value"));
    assert!(go(&root, &e, &s, &["q", "--model", "x"]).expect_err("flag").contains("unknown flag --model"));
    assert!(go(&root, &e, &s, &["q", "--path", "nowhere"])
        .expect_err("path")
        .contains("--path nowhere: no such"));
    let p = parse(&[
        "a".into(),
        "b".into(),
        "-s".into(),
        "X".into(),
        "-p".into(),
        "api".into(),
        "--steps".into(),
        "3".into(),
    ])
    .expect("p");
    assert_eq!(
        (p.question.as_str(), p.symbols, p.scope, p.steps),
        ("a b", vec!["X".to_string()], vec!["api".to_string()], 3)
    );
    assert_eq!(parse(&["a".into()]).expect("dot").scope, vec![".".to_string()]);
}

// Checks lookups are read from a reply, decorations ignored, at most three per turn.
#[test]
fn test_parse_lookups() {
    let l =
        parse_lookups("- READ a.rs:1-9\n`REFS Handle`\nfind needle here\nOUTLINE\nOUTLINE b.rs\nREAD c.rs");
    assert_eq!(
        l,
        vec![
            Lookup::Read("a.rs:1-9".into()),
            Lookup::Refs("Handle".into()),
            Lookup::Find("needle here".into())
        ]
    );
    assert_eq!(Lookup::Outline("b.rs".into()).label(), "OUTLINE b.rs");
}

// Checks the real model path through a stand-in zazzle-llm, the dispatcher and `agent investigate`, and the MCP tool's arguments.
#[test]
fn test_investigate_through_zazzle() {
    let _g = crate::testutil::env_lock();
    let (t, root) = repo();
    let fake = crate::testutil::fake_worker(
        &t,
        "fake-llm",
        &[
            ("(nothing yet)", "READ api/Endpoint.cs"),
            ("### READ", "ANSWER\nFINDING api/Endpoint.cs:3 the key is checked"),
        ],
    );
    std::env::set_var("ZAZZLE_LLM", &fake);
    let o = crate::testutil::run_cmd(&root, &["investigate", "where?", "--path", "api"]);
    let a = crate::testutil::run_cmd(&root, &["agent", "investigate", "where?"]);
    assert!(o.text.contains("1. the key is checked\n   api/Endpoint.cs:3"), "{}", o.text);
    assert!(o.text.contains("2 of 6 turns"), "two turns: {}", o.text);
    assert!(a.text.contains("findings (1 verified)"), "{}", a.text);
    let starts = std::fs::read_to_string(t.path().join("fake-llm.starts")).expect("starts");
    assert_eq!(starts.lines().count(), 1, "zazzled's worker stays loaded across turns and investigations");
    let none = t.write("none-llm", b"#!/bin/sh\necho 'error: qwen is not downloaded' >&2\nexit 4\n");
    std::fs::set_permissions(&none, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    std::env::set_var("ZAZZLE_LLM", &none);
    let n = crate::testutil::run_cmd(&root, &["investigate", "where?"]);
    std::env::remove_var("ZAZZLE_LLM");
    assert_eq!(n.text, "error: local model: qwen is not downloaded");
    let v = serde_json::json!({"question": "where?", "paths": ["api"], "symbols": ["Handle"], "steps": 3});
    assert_eq!(
        tool_args(&v).expect("args"),
        ["investigate", "--path", "api", "--symbol", "Handle", "--steps", "3", "where?"]
    );
    assert_eq!(schema()["required"][0], "question");
    assert!(tool_args(&serde_json::json!({})).is_err());
}

// Checks a refused outline, a range read, and that evidence past the cap leaves no lookups and asks for the answer.
#[test]
fn test_investigate_ranges_refusals_and_cap() {
    let (t, root) = repo();
    let big: String = (1..=1300)
        .map(|i| format!("    var value{i} = Compute(input, {i}, \"a field that takes room\");\n"))
        .collect();
    t.write("api/Big.cs", big.as_bytes());
    let e = crate::engine::Engine::new();
    let turn = |k: usize| {
        (0..3)
            .map(|i| format!("READ api/Big.cs:{}-{}", k * 3 + i * 400 + 1, k * 3 + i * 400 + 400))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (t0, t1, t2) = (turn(0), turn(1), turn(2));
    let s = Script::new(vec![
        Ok("OUTLINE sim/Fake.cs\nREAD api/Endpoint.cs:2-3\nREAD api/Endpoint.cs:x"),
        Ok(&t0),
        Ok(&t1),
        Ok(&t2),
        Ok("ANSWER\nUNKNOWN nothing more"),
    ]);
    let o = go(&root, &e, &s, &["q", "--path", "api", "--steps", "12"]).expect("run");
    let seen = s.seen.borrow();
    assert!(
        seen[1].1.contains("### OUTLINE sim/Fake.cs\nrefused:")
            && seen[1].1.contains("2\t  void Handle(Request r) {"),
        "{}",
        seen[1].1
    );
    assert!(seen[1].1.contains("### READ api/Endpoint.cs:x\nrefused:"), "{}", seen[1].1);
    let last = &seen.last().expect("last").1;
    assert!(last.contains("No lookups left: reply with ANSWER now."), "the cap ends the lookups early");
    assert!(seen.len() < 12 && o.text.contains("complete."), "{}", o.text);
}

// Regression for the logged seed miss: the starting symbol is not in the scope, the model kept asking for it, and the run ended with nothing. Now the miss is resolved before inference with real candidates, repeated failed lookups are not run again, and a seed with nothing like it returns a clear no-evidence result without asking the model.
#[test]
fn test_investigate_regression_seed_miss() {
    let (t, root) = repo();
    t.write(
        "api/WashSubscriptionService.cs",
        b"class WashSubscriptionService {\n  bool AuthorizeWash(int id) {\n    return Core.Handoff(id);\n  }\n}\n",
    );
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![
        Ok("REFS AuthorizeWashBySubscription\nFIND AuthorizeWashBySubscription"),
        Ok("REFS AuthorizeWashBySubscription\nFIND AuthorizeWashBySubscription"),
        Ok("REFS AuthorizeWashBySubscription"),
    ]);
    let o = go(&root, &e, &s, &["Who calls it?", "--path", "api", "--symbol", "AuthorizeWashBySubscription"])
        .expect("run");
    let seen = s.seen.borrow();
    assert!(
        seen[0].1.contains("### SEED AuthorizeWashBySubscription\nAuthorizeWashBySubscription is not inside the scope. Real files and symbols there that resemble it:\nfile api/WashSubscriptionService.cs\nsymbol AuthorizeWash (api/WashSubscriptionService.cs:2)"),
        "{}",
        seen[0].1
    );
    assert!(
        seen[1].1.contains("Not run again: REFS AuthorizeWashBySubscription (it already failed: no references to AuthorizeWashBySubscription"),
        "{}",
        seen[1].1
    );
    assert_eq!(seen.len(), 3, "two turns of only repeats end the run");
    let text = &o.text;
    assert!(text.contains("starting symbol not in scope: AuthorizeWashBySubscription (candidates: file api/WashSubscriptionService.cs; symbol AuthorizeWash (api/WashSubscriptionService.cs:2)"), "{text}");
    assert!(text.contains("2 lookups (REFS AuthorizeWashBySubscription; FIND AuthorizeWashBySubscription), 4 repeated lookups not run again; INCOMPLETE: it asked only for lookups it had already made, twice"), "{text}");
    let silent = Script::new(vec![Ok("ANSWER\nUNKNOWN should not be asked")]);
    let none = go(&root, &e, &silent, &["q", "--path", "api", "--symbol", "Zebra"]).expect("none");
    assert!(silent.seen.borrow().is_empty(), "no evidence, so the model is not asked");
    assert!(
        none.text.contains("no evidence: none of the starting symbols (Zebra) occurs inside the scope"),
        "{}",
        none.text
    );
    let both = Script::new(vec![Ok("ANSWER\nFINDING api/Endpoint.cs:2 Handle is here")]);
    let o = go(&root, &e, &both, &["q", "--path", "api", "--symbol", "Zebra", "--symbol", "Handle"])
        .expect("both");
    assert!(
        o.text.contains("findings (1 verified)")
            && o.text.contains("Zebra (candidates: nothing in scope resembles it)"),
        "{}",
        o.text
    );
}

// Regression for the logged empty answer: the model read the right file, then answered with nothing, and the caller got no findings and no evidence. Now it gets one repair turn; if that is empty too, the report is PARTIAL and carries the evidence verbatim.
#[test]
fn test_investigate_regression_empty_answer() {
    let (_t, root) = repo();
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![Ok("READ api/Endpoint.cs\nFIND Nowhere"), Ok("ANSWER"), Ok("")]);
    let o = go(&root, &e, &s, &["Where is the key checked?", "--path", "api"]).expect("run");
    let seen = s.seen.borrow();
    assert_eq!(seen.len(), 3, "one repair turn, no more");
    assert!(
        seen[2].1.contains("### NOTE\nYour answer was empty") && seen[2].1.contains("No lookups left"),
        "{}",
        seen[2].1
    );
    let text = &o.text;
    assert!(
        text.contains("evidence gathered (lookup output, verbatim):\n### READ api/Endpoint.cs\n"),
        "{text}"
    );
    assert!(
        text.contains("throw new Missing();") && !text.contains("### FIND Nowhere"),
        "only lookups that found something: {text}"
    );
    assert!(text.contains("3 of 6 turns") && text.contains("PARTIAL: the local model gave no answer after 1 lookups that found evidence, and none after one repair turn (it began: an empty reply); the evidence it gathered is above, verbatim"), "{text}");
    let fixed = Script::new(vec![
        Ok("READ api/Endpoint.cs"),
        Ok(""),
        Ok("ANSWER\nFINDING api/Endpoint.cs:3 a request without a key is refused"),
    ]);
    let o = go(&root, &e, &fixed, &["q", "--path", "api"]).expect("repaired");
    assert!(
        o.text.contains("findings (1 verified)")
            && o.text.contains("complete.")
            && !o.text.contains("evidence gathered"),
        "{}",
        o.text
    );
    let failed = Script::new(vec![Ok("READ api/Endpoint.cs"), Ok("ANSWER"), Err("local model: timeout")]);
    let o = go(&root, &e, &failed, &["q", "--path", "api"]).expect("failed repair");
    assert!(
        o.text.contains(
            "PARTIAL: the local model gave an empty answer, and the repair turn failed: local model: timeout"
        ),
        "{}",
        o.text
    );
    let nothing = Script::new(vec![Ok("FIND Nowhere"), Ok("ANSWER")]);
    let o = go(&root, &e, &nothing, &["q", "--path", "api"]).expect("no evidence");
    assert!(
        o.text.contains("complete.") && !o.text.contains("PARTIAL"),
        "without evidence an empty answer is just empty: {}",
        o.text
    );
}

// Checks a repeated successful read is not run again, a lookup that errors counts as failed, a repair turn that asks for lookups ends PARTIAL, and the evidence is capped at whole lookups.
#[test]
fn test_investigate_repeats_errors_and_evidence_cap() {
    let (t, root) = repo();
    let big: String = (1..=900)
        .map(|i| format!("    var value{i} = Compute(input, {i}, \"a field that takes room\");\n"))
        .collect();
    t.write("api/Big.cs", big.as_bytes());
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![
        Ok("READ api/Endpoint.cs\nREFS a|b"),
        Ok("READ api/Endpoint.cs\nREAD api/Big.cs:1-150\nREAD api/Big.cs:151-300"),
        Ok("READ api/Big.cs:301-450\nREAD api/Big.cs:451-600\nREAD api/Big.cs:601-750"),
        Ok("ANSWER"),
        Ok("READ api/Big.cs:1-10"),
    ]);
    let o = go(&root, &e, &s, &["q", "--path", "api"]).expect("run");
    let seen = s.seen.borrow();
    assert!(seen[1].1.contains("### REFS a|b\nerror:"), "{}", seen[1].1);
    assert!(
        seen[2].1.contains("Not run again: READ api/Endpoint.cs (its output is already above)"),
        "{}",
        seen[2].1
    );
    let text = &o.text;
    assert!(text.contains("PARTIAL: the local model gave no answer after 6 lookups that found evidence, and none after one repair turn (it began: \"READ api/Big.cs:1-10\")"), "{text}");
    assert!(
        text.contains("### READ api/Endpoint.cs\n") && text.contains("### READ api/Big.cs:1-150\n"),
        "{text}"
    );
    assert!(
        text.contains("(over ~6000 tokens, not shown: ") && text.contains("READ api/Big.cs:601-750)"),
        "{text}"
    );
}
