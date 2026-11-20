// Tests for investigate with a scripted local model: lookups, scope, citations, rules kept apart, and the ways a run stops.
use super::*;
use crate::testutil::TempDir;
use std::cell::RefCell;
use std::time::{Duration, Instant};

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
            .map(|r| {
                r.map(|t| Reply { text: t.into(), truncated: t.contains("CUT"), tokens: 0, queued_ms: 0 })
                    .map_err(String::from)
            })
            .collect();
        Script { replies: RefCell::new(replies), seen: RefCell::new(Vec::new()) }
    }
}

impl Local for Script {
    // Gives the next scripted reply.
    fn ask(&self, system: &str, transcript: &str, _deadline: Instant) -> Result<Reply, String> {
        self.seen.borrow_mut().push((system.into(), transcript.into()));
        self.replies
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Ok(Reply { text: String::new(), truncated: false, tokens: 0, queued_ms: 0 }))
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
    let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let deadline = Instant::now() + Duration::from_secs(parse(&argv)?.seconds);
    investigate(&ctx, &argv, s, deadline)
}

// Checks a full run: the starting symbol's refs stay inside the scope and its declaration is read before the first turn, a lookup outside it is refused, a cited finding comes back with its literal lines, a bad citation is rejected, and the rules reach the model apart from the evidence and the caller as notes.
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
    assert!(text.ends_with("-- local model, 2 of 6 turns, 4 lookups (REFS Handle; READ api/Endpoint.cs:2-5; READ api/Endpoint.cs; READ sim/Fake.cs); complete. Excerpts are read from the files; claims are the model's."), "{text}");
    let seen = s.seen.borrow();
    let (first_system, first) = &seen[0];
    assert!(
        first.contains("### E1 REFS Handle")
            && first.contains("### E2 READ api/Endpoint.cs:2-5\n")
            && first.contains("if (r.Key == null) throw new Missing();")
            && !first.contains("sim/Fake.cs"),
        "the declaration is read before the first turn: {first}"
    );
    assert!(
        first_system.contains("published contract"),
        "the declaration's read brings its rules: {first_system}"
    );
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
    assert!(o.text.contains("PARTIAL: it gave no ANSWER when its lookups ran out (its reply began: \"READ api/Endpoint.cs\"), and its answer-only turn gave none (it began: an empty reply); ranked excerpts of its evidence are above, verbatim"), "{}", o.text);
    assert!(
        o.text.contains("### E1 FIND Store\napi/Endpoint.cs\n4:     Store(r);\n"),
        "the evidence comes back: {}",
        o.text
    );
    assert!(o.text.contains("2 of 2 turns and an answer-only turn"), "{}", o.text);
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

// Checks the real model path through a stand-in ai-agent, the dispatcher and `agent investigate`, and the MCP tool's arguments.
#[test]
fn test_investigate_through_zazzle() {
    let _g = crate::testutil::env_lock();
    let (t, root) = repo();
    let fake = crate::testutil::fake_worker(
        &t,
        "fake-llm",
        &[
            ("(nothing yet)", "READ api/Endpoint.cs"),
            ("### E1 READ", "ANSWER\nFINDING api/Endpoint.cs:3 the key is checked"),
        ],
    );
    std::env::set_var("ZAZZLE_LLM", &fake);
    let o = crate::testutil::run_cmd(&root, &["investigate", "where?", "--path", "api"]);
    let a = crate::testutil::run_cmd(&root, &["agent", "investigate", "where?"]);
    assert!(o.text.contains("1. the key is checked\n   api/Endpoint.cs:3"), "{}", o.text);
    assert!(o.text.contains("2 of 6 turns"), "two turns: {}", o.text);
    assert!(a.text.contains("findings (1 verified)"), "{}", a.text);
    let starts = std::fs::read_to_string(t.path().join("fake-llm.starts")).expect("starts");
    assert_eq!(starts.lines().count(), 1, "daemon's worker stays loaded across turns and investigations");
    let none = t.write("none-llm", b"#!/bin/sh\necho 'error: qwen is not downloaded' >&2\nexit 4\n");
    std::fs::set_permissions(&none, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    std::env::set_var("ZAZZLE_LLM", &none);
    let n = crate::testutil::run_cmd(&root, &["investigate", "where?"]);
    std::env::remove_var("ZAZZLE_LLM");
    assert_eq!(n.text, "error: local model: qwen is not downloaded");
    let v = serde_json::json!({"question": "where?", "paths": ["api"], "symbols": ["Handle"], "steps": 3, "seconds": 60});
    assert_eq!(
        tool_args(&v).expect("args"),
        ["investigate", "--path", "api", "--symbol", "Handle", "--steps", "3", "--seconds", "60", "where?"]
    );
    assert!(schema()["properties"]["run_id"].is_object());
    assert!(tool_args(&serde_json::json!({})).is_err());
    assert_eq!(
        tool_args(&serde_json::json!({"run_id": "r1"})).expect("lookup"),
        ["investigate", "--run-id", "r1"]
    );
    assert_eq!(
        tool_args(&serde_json::json!({"run_id": "r1", "question": "q"})).expect("resubmit"),
        ["investigate", "--run-id", "r1", "q"]
    );
    assert!(tool_args(&serde_json::json!({"run_id": "r1", "steps": 2})).is_err());
    let v = serde_json::json!({"question": "q", "adaptive": true, "prose": true, "agents": ["qwen", "claude:opus@high"], "benchmark_model": "codex:gpt", "review_effort": "high"});
    assert_eq!(
        tool_args(&v).expect("adaptive args"),
        [
            "investigate",
            "--adaptive",
            "--prose",
            "--agents",
            "qwen,claude:opus@high",
            "--benchmark-model",
            "codex:gpt",
            "--review-effort",
            "high",
            "q"
        ]
    );
    assert!(tool_args(&serde_json::json!({"question": "q", "review_model": 3})).is_err());
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
    assert!(seen.len() < 12, "{}", o.text);
    assert!(
        o.text.contains(
            "PARTIAL: no finding could be verified; ranked excerpts of its evidence are above, verbatim"
        ) && o.text.contains("### E1 READ api/Endpoint.cs:2-3\n"),
        "an answer of only unknowns still returns the evidence: {}",
        o.text
    );
    assert!(!o.text.contains("var value300 "), "the excerpts stay short: {}", o.text);
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

// Regression for the logged empty answer: the model read the right file, then answered with nothing, and the caller got no findings and no evidence. Now it gets one answer-only turn; if that is empty too, the report is PARTIAL and carries ranked excerpts of the evidence verbatim, with the lookups that found nothing as gaps.
#[test]
fn test_investigate_regression_empty_answer() {
    let (_t, root) = repo();
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![Ok("READ api/Endpoint.cs\nFIND Nowhere"), Ok("ANSWER"), Ok("")]);
    let o = go(&root, &e, &s, &["Where is the key checked?", "--path", "api"]).expect("run");
    let seen = s.seen.borrow();
    assert_eq!(seen.len(), 3, "one answer-only turn, no more");
    assert!(
        seen[2].1.contains("### NOTE\nYour answer was empty") && seen[2].1.contains("No lookups left"),
        "{}",
        seen[2].1
    );
    let text = &o.text;
    assert!(
        text.contains("evidence (no finding verified; ranked excerpts of the lookups, verbatim):\n### E1 READ api/Endpoint.cs\n"),
        "{text}"
    );
    assert!(
        text.contains("3\t    if (r.Key == null) throw new Missing();") && !text.contains("### E2"),
        "only lookups that found something, around the question's words: {text}"
    );
    assert!(text.contains("lookups that found nothing:\n- FIND Nowhere: no matches in"), "{text}");
    assert!(text.contains("2 of 6 turns and an answer-only turn") && text.contains("PARTIAL: its answer was empty after 1 lookups that found evidence, and its answer-only turn gave none (it began: an empty reply); ranked excerpts of its evidence are above, verbatim"), "{text}");
    let fixed = Script::new(vec![
        Ok("READ api/Endpoint.cs"),
        Ok(""),
        Ok("ANSWER\nFINDING api/Endpoint.cs:3 a request without a key is refused"),
    ]);
    let o = go(&root, &e, &fixed, &["q", "--path", "api"]).expect("repaired");
    assert!(
        o.text.contains("findings (1 verified)")
            && o.text.contains("complete, on an answer-only turn after its answer was empty")
            && !o.text.contains("ranked excerpts"),
        "{}",
        o.text
    );
    let failed = Script::new(vec![Ok("READ api/Endpoint.cs"), Ok("ANSWER"), Err("local model: timeout")]);
    let o = go(&root, &e, &failed, &["q", "--path", "api"]).expect("failed repair");
    assert!(
        o.text.contains("PARTIAL: its answer was empty after 1 lookups that found evidence, and its answer-only turn failed: local model: timeout"),
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

// Checks a repeated successful read is not run again, a lookup that errors counts as failed, an answer-only turn that asks for lookups ends PARTIAL, and the excerpts are capped at whole lookups.
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
    assert!(text.contains("PARTIAL: its answer was empty after 6 lookups that found evidence, and its answer-only turn gave none (it began: \"READ api/Big.cs:1-10\")"), "{text}");
    assert!(
        text.contains("### E1 READ api/Endpoint.cs\n") && text.contains("### E2 READ api/Big.cs:1-150\n"),
        "{text}"
    );
    assert!(
        text.contains("### E6 READ api/Big.cs:601-750\n") && !text.contains("var value140 "),
        "no line names the question, so each lookup gives only its first lines: {text}"
    );
    assert!(text.contains("lookups that found nothing:\n- REFS a|b: error:"), "{text}");
}
// Checks the excerpts: lookups whose lines name the question's words rank first and keep those lines with their
// context, a file's line is shown once across lookups, session markers are dropped, a lookup with no matching line
// gives its first lines, and the rest past the token cap is named.
#[test]
fn test_excerpts_rank_dedupe_and_cap() {
    let plan =
        parse(&["Where is the retry budget spent?".to_string(), "--symbol".into(), "SpendRetry".into()])
            .expect("plan");
    let numbered = |from: usize, to: usize, hit: usize| -> String {
        let mut t = String::from("[r:abc12]\nsvc/Retry.cs 400 lines, sha 1\n");
        for k in from..=to {
            let word = if k == hit { "budget" } else { "other" };
            t.push_str(&format!("{k}\t  let {word}{k} = {k};\n"));
        }
        t
    };
    let quiet: String = (1..=30).map(|k| format!("{k}\tnothing here {k}\n")).collect();
    let mut trail = Trail {
        seen: vec![
            ("READ svc/Quiet.cs".into(), format!("svc/Quiet.cs 30 lines, sha 2\n{quiet}")),
            ("READ svc/Retry.cs:1-40".into(), numbered(1, 40, 20)),
            ("NOTE".into(), "ignored".into()),
            ("READ svc/Retry.cs:15-25".into(), numbered(15, 25, 20)),
        ],
        rules: String::new(),
        lookups: Vec::new(),
        done: Vec::new(),
        evidence: vec![0, 1, 3],
        skipped: 0,
    };
    let out = excerpts(&trail, &plan);
    assert!(out.starts_with("evidence (no finding verified; ranked excerpts of the lookups, verbatim):\n### E2 READ svc/Retry.cs:1-40\n"), "{out}");
    assert!(
        out.contains("…\n18\t  let other18 = 18;\n19\t")
            && out.contains("20\t  let budget20 = 20;\n21\t")
            && out.contains("22\t  let other22 = 22;\n"),
        "{out}"
    );
    assert!(!out.contains("17\t") && !out.contains("23\t") && !out.contains("[r:abc12]"), "{out}");
    assert!(
        out.contains("### E1 READ svc/Quiet.cs\n")
            && out.contains("11\tnothing here 11")
            && !out.contains("12\tnothing here 12"),
        "{out}"
    );
    assert!(!out.contains("### E3"), "its lines were all shown under E2: {out}");
    let big: String =
        (1..=4000).map(|k| format!("{k}\tbudget {k} of a long line that takes room\n")).collect();
    trail.seen.push(("READ svc/Big.cs".into(), format!("svc/Big.cs 4000 lines\n{big}")));
    trail.evidence.insert(0, 4);
    let capped = excerpts(&trail, &plan);
    assert!(capped.contains("(over ~3000 tokens, not shown: "), "{capped}");
    assert_eq!(terms(&plan), ["spendretry", "spend", "retry", "budget", "spent"]);
}

// Regression for the logged idempotency run: the model guessed a file that does not exist, retried it with a line
// range, and repeated lookups until the run stopped with no findings and no evidence. Now the guess gets the real files
// that resemble it, the ranged retry counts as a repeat, and the stop gives one answer-only turn and then the evidence.
#[test]
fn test_investigate_regression_missing_path_and_repeats() {
    let (t, root) = repo();
    t.write(
        "api/IdempotencyKeyFilter.cs",
        b"class IdempotencyKeyFilter {\n  bool Seen(string key) => false;\n}\n",
    );
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![
        Ok("READ api/Endpoint.cs\nREAD api/IdempotencyKeyRepository.cs"),
        Ok("READ api/IdempotencyKeyRepository.cs:1-40"),
        Ok("OUTLINE api/IdempotencyKeyRepository.cs\nREAD api/Endpoint.cs"),
        Ok(""),
    ]);
    let o = go(&root, &e, &s, &["Where is the idempotency key checked?", "--path", "api"]).expect("run");
    let seen = s.seen.borrow();
    assert!(seen[1].1.contains("refused: api/IdempotencyKeyRepository.cs does not exist inside the scope (api). Real files and symbols there that resemble it:\n") && seen[1].1.contains("IdempotencyKeyFilter"), "{}", seen[1].1);
    assert!(seen[2].1.contains("Not run again: READ api/IdempotencyKeyRepository.cs:1-40 (it already failed: refused: api/IdempotencyKeyRepository.cs does not exist"), "{}", seen[2].1);
    assert_eq!(seen.len(), 4, "three turns and one answer-only turn");
    assert!(
        seen[3].1.contains("You asked only for lookups that had already run. No more lookups will run."),
        "{}",
        seen[3].1
    );
    let text = &o.text;
    assert!(text.contains("### E1 READ api/Endpoint.cs\n") && text.contains("r.Key == null"), "{text}");
    assert!(
        text.contains("lookups that found nothing:\n- READ api/IdempotencyKeyRepository.cs: refused:"),
        "{text}"
    );
    assert!(text.contains("PARTIAL: it asked only for lookups it had already made, twice, and its answer-only turn gave none"), "{text}");
    assert!(text.contains("3 repeated lookups not run again"), "{text}");
}

// Regression for the logged trace run: on its last turn the model asked for another READ, and the run ended with no
// findings. Now the answer-only turn after it can answer, citing an excerpt by its id.
#[test]
fn test_investigate_regression_final_turn_lookup() {
    let (_t, root) = repo();
    let e = crate::engine::Engine::new();
    let s = Script::new(vec![
        Ok("READ api/Endpoint.cs"),
        Ok("READ sim/Fake.cs"),
        Ok("ANSWER\nFINDING E1:3 a request without a key is refused"),
    ]);
    let o = go(&root, &e, &s, &["q", "--path", "api", "--steps", "2"]).expect("run");
    let seen = s.seen.borrow();
    assert!(
        seen[2]
            .1
            .contains("### NOTE\nYou asked for more lookups, but none were left. No more lookups will run."),
        "{}",
        seen[2].1
    );
    assert!(
        o.text.contains("1. a request without a key is refused\n   api/Endpoint.cs:3\n"),
        "E1 resolves to its file: {}",
        o.text
    );
    assert!(
        o.text.contains("complete, on an answer-only turn after it gave no ANSWER when its lookups ran out"),
        "{}",
        o.text
    );
    assert!(!o.text.contains("ranked excerpts"), "{}", o.text);
    let stray = Script::new(vec![Ok("READ api/Endpoint.cs"), Ok("ANSWER\nFINDING E9:1 no such excerpt")]);
    let o = go(&root, &e, &stray, &["q", "--path", "api"]).expect("stray id");
    assert!(
        o.text.contains("rejected citations:\n- E9:1 no such excerpt:") && o.text.contains("### E1 READ"),
        "{}",
        o.text
    );
}

// A local model that takes longer than the investigation's time limit on its first turn.
struct Slow;

impl Local for Slow {
    // Asks for a lookup after the deadline has passed.
    fn ask(&self, _system: &str, _transcript: &str, deadline: Instant) -> Result<Reply, String> {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20));
        Ok(Reply { text: "READ api/Endpoint.cs".into(), truncated: false, tokens: 0, queued_ms: 0 })
    }
}

// Checks the time limit: a turn that runs past it ends the run before the lookup it asked for and the next turn, without an answer-only turn, and the
// evidence gathered so far (the starting symbol's refs and declaration) comes back.
#[test]
fn test_investigate_deadline() {
    let (_t, root) = repo();
    let e = crate::engine::Engine::new();
    let ctx = Ctx {
        engine: &e,
        cwd: root.clone(),
        session: String::new(),
        roots: Vec::new(),
        stdin: RefCell::new(None),
        caller: RefCell::new(None),
    };
    let argv: Vec<String> = ["q", "--path", "api", "--symbol", "Handle", "--seconds", "1"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    let o = investigate(&ctx, &argv, &Slow, Instant::now() + Duration::from_secs(1)).expect("run");
    assert!(
        o.text.contains("PARTIAL: its 1 s time limit ran out before READ api/Endpoint.cs; ranked excerpts of its evidence are above"),
        "{}",
        o.text
    );
    assert!(o.text.contains("1 of 6 turns,") && o.text.contains("### E1 REFS Handle"), "{}", o.text);
    assert!(parse(&["q".into(), "--seconds".into(), "0".into()]).is_err());
}

// Checks the declarations read for a seed come from the refs output's defs only, at most two, and that an excerpt id
// names a file only for a READ or OUTLINE that found something.
#[test]
fn test_defs_and_excerpt_ids() {
    let refs = "defs\napi/A.cs:3-9 method Handle\napi/B.cs:1-4 class B\napi/C.cs:2-3 field C\nrefs\napi/D.cs\n5 [x] Handle();\n";
    assert_eq!(defs(refs), ["api/A.cs:3-9", "api/B.cs:1-4"]);
    assert!(defs("refs\napi/D.cs\n5 [x] Handle();\n").is_empty());
    assert!(
        defs("defs\napi/A.cs:3 method Handle\n-- 1 defs").is_empty(),
        "a def without a range is not read"
    );
    let trail = Trail {
        seen: vec![
            ("REFS Handle".into(), "defs".into()),
            ("NOTE".into(), "x".into()),
            ("OUTLINE api/A.cs".into(), "1-9 class A".into()),
            ("READ api/B.cs:1-4".into(), "1\tclass B".into()),
        ],
        rules: String::new(),
        lookups: Vec::new(),
        done: Vec::new(),
        evidence: vec![0, 2, 3],
        skipped: 0,
    };
    assert_eq!(trail.file_of("E1"), None, "a REFS excerpt has no single file");
    assert_eq!(trail.file_of("E2").as_deref(), Some("api/A.cs"));
    assert_eq!(trail.file_of("E3").as_deref(), Some("api/B.cs"));
    assert_eq!(
        (trail.file_of("E0"), trail.file_of("E4"), trail.file_of("api/B.cs"), trail.file_of("Ex")),
        (None, None, None, None)
    );
    assert_eq!((trail.id(1), trail.id(2)), (String::new(), "E2 ".to_string()));
}

// Checks a slow exact-reference lookup during seed resolution ends with the job's deadline: the indexer request is cut off (the shared indexer is left running and not marked broken), the text search answers instead, and no model turn starts after the deadline.
#[test]
fn test_slow_seed_lookup_ends_at_the_deadline() {
    let _g = crate::testutil::env_lock();
    let fake = crate::indexer::tests::FakeIndexer::start(|_, req| {
        if !req.is_null() {
            std::thread::sleep(Duration::from_secs(5));
        }
        let text = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"content": [{"type": "text", "text": "late"}]}});
        (serde_json::json!({"ok": true}), Some(text))
    });
    std::env::set_var("ZAZZLE_INDEXER", &fake.exe);
    std::env::set_var("ZAZZLE_INDEXER_STATE_DIR", fake.dir.path());
    let (_t, root) = repo();
    let e = crate::engine::Engine::new();
    let ctx = Ctx {
        engine: &e,
        cwd: root.clone(),
        session: String::new(),
        roots: Vec::new(),
        stdin: RefCell::new(None),
        caller: RefCell::new(None),
    };
    let s = Script::new(vec![Ok("ANSWER\nUNKNOWN nothing")]);
    let argv: Vec<String> = ["where?", "--path", "api", "--symbol", "Handle", "--seconds", "60"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    let t0 = Instant::now();
    let o = investigate(&ctx, &argv, &s, t0 + Duration::from_millis(600));
    std::env::remove_var("ZAZZLE_INDEXER");
    std::env::remove_var("ZAZZLE_INDEXER_STATE_DIR");
    let o = o.expect("run");
    assert!(
        t0.elapsed() < Duration::from_millis(1500),
        "ended at the deadline, not the indexer's 45 s: {:?}",
        t0.elapsed()
    );
    assert!(s.seen.borrow().is_empty(), "no model turn starts after the deadline");
    assert!(
        o.text.contains("exact C# refs unavailable")
            && o.text.contains("the job's time limit ran out while the indexer answered")
            && o.text.contains("the indexer keeps running"),
        "{}",
        o.text
    );
    assert!(o.text.contains("time limit ran out") && o.text.contains("api/Endpoint.cs:2"), "{}", o.text);
}

// Checks run ids on the command line (`--run-id ID`, `--run-id=ID`), the refusals that need no model (no id value, no store, an unknown id, nothing asked), and that no seed lookup starts after the deadline.
#[test]
fn test_run_ids_and_refusals() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(take_run_id(&v(&["q", "--run-id=a1"])).expect("eq"), (Some("a1".to_string()), v(&["q"])));
    assert_eq!(take_run_id(&v(&["--run-id", "b2", "q"])).expect("sep"), (Some("b2".to_string()), v(&["q"])));
    assert!(take_run_id(&v(&["q", "--run-id"])).is_err());
    assert_eq!(seconds_of(&v(&["q", "--seconds", "7"])), 7);
    assert_eq!(seconds_of(&v(&["q", "--seconds", "x"])), DEFAULT_SECONDS);
    let (t, root) = repo();
    let ctx_with = |e: &crate::engine::Engine| -> Result<Output, String> {
        let ctx = Ctx {
            engine: e,
            cwd: root.clone(),
            session: String::new(),
            roots: Vec::new(),
            stdin: RefCell::new(None),
            caller: RefCell::new(None),
        };
        run(&ctx, &v(&["--run-id", "nope"]))
    };
    let stored = crate::engine::Engine::new().with_runs(Some(t.path().join("runs")));
    assert_eq!(ctx_with(&stored).unwrap_err(), "no investigate run nope");
    assert!(ctx_with(&crate::engine::Engine::new()).unwrap_err().contains("--run-id needs the run store"));
    let ctx = Ctx {
        engine: &stored,
        cwd: root.clone(),
        session: String::new(),
        roots: Vec::new(),
        stdin: RefCell::new(None),
        caller: RefCell::new(None),
    };
    assert_eq!(run(&ctx, &[]).unwrap_err(), USAGE);
    let s = Script::new(vec![]);
    let e = explore(&ctx, &v(&["q", "--symbol", "Handle", "--path", "api"]), &s, Instant::now())
        .expect("explore");
    assert!(
        e.trail.lookups.is_empty() && s.seen.borrow().is_empty(),
        "nothing starts after the deadline: {:?}",
        e.trail.lookups
    );
}
