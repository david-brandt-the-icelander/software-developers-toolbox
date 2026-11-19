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
    let fake = t.write(
        "fake-llm",
        b"#!/bin/sh\ncat >/dev/null\nprintf 'ANSWER\\nFINDING api/Endpoint.cs:3 the key is checked\\n'\n",
    );
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    std::env::set_var("ZAZZLE_LLM", &fake);
    let o = crate::testutil::run_cmd(&root, &["investigate", "where?", "--path", "api"]);
    let a = crate::testutil::run_cmd(&root, &["agent", "investigate", "where?"]);
    std::env::remove_var("ZAZZLE_LLM");
    assert!(o.text.contains("1. the key is checked\n   api/Endpoint.cs:3"), "{}", o.text);
    assert!(a.text.contains("findings (1 verified)"), "{}", a.text);
    let v = serde_json::json!({"question": "where?", "paths": ["api"], "symbols": ["Handle"], "steps": 3});
    assert_eq!(
        tool_args(&v).expect("args"),
        ["investigate", "--path", "api", "--symbol", "Handle", "--steps", "3", "where?"]
    );
    assert_eq!(schema()["required"][0], "question");
    assert!(tool_args(&serde_json::json!({})).is_err());
}
