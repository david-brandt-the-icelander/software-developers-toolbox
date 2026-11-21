// Tests for the adaptive mode with scripted agents: options, assessment, planning, escalation with handoff, combining, the summary, the review, the benchmark, and the real command lines through stand-in programs.
use super::*;
use crate::testutil::TempDir;
use agent::Said;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

// Agents that reply from a script per rung label (`qwen-a low`, `claude:sonnet high`), recording each rung's prompts.
#[derive(Default)]
struct Fake {
    replies: Mutex<HashMap<String, VecDeque<Result<String, String>>>>,
    seen: Mutex<Vec<(String, String)>>,
    installed: Vec<String>,
    hosts: Vec<agent::Kind>,
    records: Mutex<Vec<Value>>,
}

impl Fake {
    // Builds a script from (rung label, reply) pairs, in the order each rung is asked.
    fn new(script: &[(&str, Result<&str, &str>)]) -> Fake {
        let f = Fake::default();
        for (who, r) in script {
            let r = r.map(String::from).map_err(String::from);
            f.replies.lock().expect("lock").entry(who.to_string()).or_default().push_back(r);
        }
        f
    }

    // The next reply for a rung, recording its prompt; an unscripted call is an error.
    fn next(&self, who: String, prompt: String) -> Result<String, String> {
        self.seen.lock().expect("lock").push((who.clone(), prompt));
        let mut r = self.replies.lock().expect("lock");
        r.get_mut(&who)
            .and_then(|q| q.pop_front())
            .unwrap_or_else(|| Err(format!("no reply scripted for {who}")))
    }

    // The prompts one rung was given.
    fn prompts(&self, who: &str) -> Vec<String> {
        self.seen.lock().expect("lock").iter().filter(|(w, _)| w == who).map(|(_, p)| p.clone()).collect()
    }
}

// Plays a scripted reply that begins `SLEEP MS` like a real agent: it takes that long, but is stopped at the deadline or a cancel.
fn paced(reply: Result<String, String>, deadline: Instant) -> Result<String, String> {
    let Some((ms, rest)) =
        reply.as_ref().ok().and_then(|t| t.strip_prefix("SLEEP ")).and_then(|t| t.split_once('\n'))
    else {
        return reply;
    };
    let end = Instant::now() + Duration::from_millis(ms.parse().unwrap_or(0));
    while Instant::now() < end.min(deadline) && !crate::budget::cancelled() {
        std::thread::sleep(Duration::from_millis(10));
    }
    match (crate::budget::cancelled(), end > deadline) {
        (true, _) => Err("stopped at the cancel".into()),
        (false, true) => Err("stopped at the time limit".into()),
        (false, false) => Ok(rest.to_string()),
    }
}

impl Backend for Fake {
    // Replies for `MODEL EFFORT`, with seven tokens.
    fn local(&self, model: &str, req: &Value, deadline: Instant) -> Result<Reply, String> {
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
        let prompt = match req.get("grammar") {
            Some(_) => format!("{prompt}\n[held to a grammar]"),
            None => prompt,
        };
        paced(self.next(r.label(), prompt), deadline).map(|text| Reply {
            truncated: text.contains("CUT"),
            text,
            tokens: 7,
            queued_ms: 0,
        })
    }

    // Replies for the hosted rung, with eleven tokens and a cent.
    fn hosted(
        &self,
        rung: &Rung,
        prompt: &str,
        _cwd: &Path,
        deadline: Instant,
        _write: bool,
    ) -> Result<Said, String> {
        paced(self.next(rung.label(), prompt.into()), deadline).map(|text| Said {
            text,
            tokens: 11,
            input: 100,
            cached: 40,
            rounds: 2,
            cost: Some(0.01),
            counted: true,
        })
    }

    // The scripted installed models.
    fn installed(&self) -> Vec<String> {
        self.installed.clone()
    }

    // The scripted hosted command lines this machine has.
    fn can_host(&self, kind: agent::Kind) -> bool {
        self.hosts.contains(&kind)
    }

    // Keeps the orchestration records for the tests to read.
    fn record(&self, rec: &Value) {
        self.records.lock().expect("lock").push(rec.clone());
    }
}

// Builds a repository with an API endpoint that checks a key and a simulator.
fn repo() -> (TempDir, PathBuf) {
    let t = TempDir::new();
    let root = t.path().canonicalize().expect("root");
    std::fs::create_dir_all(root.join(".git")).expect("git");
    t.write(
        "api/Endpoint.cs",
        b"class Endpoint {\n  void Handle(Request r) {\n    if (r.Key == null) throw new Missing();\n    Store(r);\n  }\n}\n",
    );
    t.write("sim/Fake.cs", b"class Fake {\n  void Handle(Request r) { }\n}\n");
    (t, root)
}

// Runs the command line in a context on `root` with the fake agents.
fn go(root: &Path, f: &Fake, argv: &[&str]) -> Result<String, String> {
    let e = crate::engine::Engine::new();
    let ctx = Ctx {
        engine: &e,
        cwd: root.to_path_buf(),
        session: String::new(),
        roots: Vec::new(),
        stdin: RefCell::new(None),
        caller: RefCell::new(None),
    };
    run_with(&ctx, &argv.iter().map(|a| a.to_string()).collect::<Vec<_>>(), f).map(|o| o.text)
}

const KEY: &str = "ANSWER\nFINDING api/Endpoint.cs:2-3 Handle rejects a request without a key\nCONFIDENCE understanding=high result=high";

// Checks the flags are told apart from investigate's, need --adaptive where they shape it, and need their values.
#[test]
fn test_options() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(wanted(&a(&["q", "--review-model", "m"])) && !wanted(&a(&["q", "--path", "api"])));
    let o = options(&a(&[
        "q",
        "--adaptive",
        "--agents",
        "a, b,,",
        "--benchmark-effort",
        "high",
        "--review-model",
        "m",
        "--prose",
        "--json",
        "--path",
        "api",
    ]))
    .expect("options");
    assert_eq!(o.agents, ["a", "b"]);
    assert_eq!((o.bench, o.review), (Some((None, Some("high".into()))), Some((Some("m".into()), None))));
    assert!(o.adaptive && o.prose && o.json);
    assert_eq!(o.rest, ["q", "--path", "api"]);
    assert!(options(&a(&["q", "--agents", "a"])).unwrap_err().contains("add --adaptive"));
    assert!(options(&a(&["q", "--prose"])).unwrap_err().contains("add --adaptive"));
    assert!(options(&a(&["q", "--orchestrator", "claude:opus"])).unwrap_err().contains("add --adaptive"));
    assert!(options(&a(&["q", "--adaptive", "--orchestrator-effort", "high"]))
        .unwrap_err()
        .contains("needs --orchestrator"));
    let o =
        options(&a(&["q", "--adaptive", "--orchestrator", "claude:opus", "--orchestrator-effort", "max"]))
            .expect("orchestrator");
    assert_eq!(o.orchestrator, (Some("claude:opus".into()), Some("max".into())));
    assert!(options(&a(&["q", "--review-effort"])).unwrap_err().contains("--review-effort needs a value"));
    let ladder = agent::ladder(&a(&["qwen-a", "claude:opus@high"])).expect("ladder");
    assert_eq!(chosen(&(None, None), &ladder).expect("top").label(), "claude:opus high");
    assert_eq!(chosen(&(None, Some("max".into())), &ladder).expect("effort").label(), "claude:opus max");
    assert!(chosen(&(None, Some("huge".into())), &ladder).unwrap_err().contains("takes effort"));
    assert_eq!(
        chosen(&(Some("codex:gpt".into()), Some("xhigh".into())), &ladder).expect("named").label(),
        "codex:gpt xhigh"
    );
    assert_eq!(chosen(&(None, None), &[]).expect("default").label(), "local default low");
}

// Checks the difficulty rating counts each reason and caps at five.
#[test]
fn test_assess() {
    let p = |args: &[&str]| {
        investigate::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).expect("plan")
    };
    assert_eq!(assess(&p(&["Where", "is", "Handle?", "--path", "api", "--symbol", "Handle"])), (0, vec![]));
    let (n, why) = assess(&p(&["Why", "is", "the", "key", "checked?", "Should", "it", "be?"]));
    assert_eq!(
        (n, why),
        (4, vec!["several questions", "the whole repository", "no starting symbol", "it asks for judgement"])
    );
    let long = "w ".repeat(20) + "; and why";
    let many = p(&[&long, "--symbol", "A", "--symbol", "B", "--symbol", "C", "--path", "api"]);
    assert_eq!(
        assess(&many).1,
        ["a long question", "several questions", "many starting symbols", "it asks for judgement"]
    );
}

// Checks a simple question skips the planner and runs as one part on the cheapest rung.
#[test]
fn test_adaptive_simple_question() {
    let (_t, root) = repo();
    let f = Fake::new(&[("qwen-a low", Ok(KEY))]);
    let out = go(
        &root,
        &f,
        &[
            "Where",
            "is",
            "Handle?",
            "--path",
            "api",
            "--symbol",
            "Handle",
            "--adaptive",
            "--agents",
            "qwen-a",
        ],
    )
    .expect("run");
    assert!(out.contains("ladder: 1 qwen-a low, 2 qwen-a high\ndifficulty: 0 of 5 (a short question about named symbols)\nplan: one part on qwen-a low, no planner call\n"), "{out}");
    assert!(
        out.contains("  P1 qwen-a low: complete, 1 verified, understanding high, result high\n"),
        "{out}"
    );
    assert!(out.contains("answer:\nfindings (1 verified):\n1. Handle rejects a request without a key\n   api/Endpoint.cs:2-3\n"), "{out}");
    assert!(out.contains("-- adaptive: complete.") && out.contains("usage: 1 calls (part 1), "), "{out}");
    assert!(
        f.prompts("qwen-a low")[0].contains("CONFIDENCE understanding=high|medium|low"),
        "the executor is asked to rate itself"
    );
}

// Checks a full adaptive run: a hosted planner one rung above the difficulty, a part escalated on low confidence with what it established, a part blocked on the user left as it is, a combination whose stray citation is rejected, the summary and the JSON report.
#[test]
fn test_adaptive_plan_escalate_combine() {
    let (_t, root) = repo();
    let script = [
        ("claude:sonnet high", Ok("PART 1 | I1 | Handle\nPART 3 | I2 |\nCONFIDENCE understanding=high result=medium")),
        ("qwen-a low", Ok("ANSWER\nFINDING api/Endpoint.cs:3 a null key throws\nCONFIDENCE understanding=high result=low")),
        ("qwen-a high", Ok(KEY)),
        ("claude:sonnet high", Ok("STATUS partial\nFINDING api/Endpoint.cs:4 the request is stored\nGAP compatibility with published versions\nBLOCKED decision which version may enforce it")),
        ("claude:sonnet high", Ok("ANSWER\nFINDING api/Endpoint.cs:2-3 the key is checked in Handle\nFINDING sim/Fake.cs:2 a stray claim\nCONFIDENCE understanding=high result=medium")),
        ("qwen-a low", Ok("Handle checks the key [1]; which version may enforce it is open.")),
    ];
    let f = Fake::new(&script);
    let q = ["Why", "is", "the", "key", "checked?", "Should", "a", "blank", "key", "be", "rejected?"];
    let mut argv = q.to_vec();
    argv.extend(["--adaptive", "--agents", "qwen-a,claude:sonnet@high", "--prose"]);
    let out = go(&root, &f, &argv).expect("run");
    assert!(
        out.contains(
            "difficulty: 4 of 5 (several questions, the whole repository, no starting symbol, it asks for judgement)\n"
        ),
        "{out}"
    );
    assert!(out.contains("plan: 2 parts by claude:sonnet high\n  P1 qwen-a low: I1: Why is the key checked?\n  P2 claude:sonnet high: I2: Should a blank key be rejected?\n"), "the parts keep the items' own words: {out}");
    let planned = &f.prompts("claude:sonnet high")[0];
    assert!(
        planned.contains("Items:\nI1: Why is the key checked?\nI2: Should a blank key be rejected?\n")
            && planned.contains("PART agent-number | item ids"),
        "{planned}"
    );
    assert!(
        out.contains(
            "  P1 qwen-a low (it rated the support for its results low) → qwen-a high: complete, 1 verified"
        ),
        "{out}"
    );
    assert!(out.contains("  P2 claude:sonnet high: partial, 1 verified"), "{out}");
    let handoff = &f.prompts("qwen-a high")[0];
    assert!(
        handoff.contains("Established by an earlier attempt")
            && handoff.contains("verified: a null key throws (api/Endpoint.cs:3)"),
        "{handoff}"
    );
    assert!(
        handoff.contains("qwen-a low did not settle it: it rated the support for its results low"),
        "{handoff}"
    );
    assert!(out.contains("answer (combined by claude:sonnet high):\nfindings (1 verified):\n1. the key is checked in Handle"), "{out}");
    assert!(out.contains("- sim/Fake.cs:2 a stray claim: no part verified these lines"), "{out}");
    assert!(out.contains("summary (by qwen-a low):\nHandle checks the key [1]"), "{out}");
    assert!(
        out.contains("usage: 6 calls (planner 1, part 2, escalation 1, combine 1, summary 1), "),
        "{out}"
    );
    assert!(out.contains("output tokens, $0.0300 reported"), "{out}");
    let combine = &f.prompts("claude:sonnet high")[2];
    assert!(
        combine.contains("F1 api/Endpoint.cs:2-3 Handle rejects") && combine.contains("GAP compatibility"),
        "{combine}"
    );

    let f = Fake::new(&script);
    argv.push("--json");
    let v: Value = serde_json::from_str(&go(&root, &f, &argv).expect("json")).expect("parse");
    assert_eq!(v["status"], "complete");
    assert_eq!(v["findings"][0]["evidence"][0], "api/Endpoint.cs:2-3");
    assert_eq!(v["planner"], "claude:sonnet high");
    assert_eq!(v["parts"][0]["attempts"][0]["escalated_because"], "it rated the support for its results low");
    assert_eq!(v["parts"][1]["attempts"][0]["result"]["needs_user"], "decision which version may enforce it");
    assert_eq!(v["parts"][1]["attempts"][0]["result"]["escalation"]["recommended"], false);
    assert_eq!(v["usage"]["calls"], 6);
}

// Checks the orchestrator plans, combines and summarizes in place of the ladder, its tokens are reported apart from the executors', and the run is logged without the question's text.
#[test]
fn test_orchestrator() {
    let (_t, root) = repo();
    let other =
        "ANSWER\nFINDING api/Endpoint.cs:4 the request is stored\nCONFIDENCE understanding=high result=high";
    let script = [
        ("claude:opus high", Ok("PART 1 | I1 | Handle\nPART 1 | I2 | Store\nCONFIDENCE understanding=high result=high")),
        ("qwen-a low", Ok(KEY)),
        ("qwen-a low", Ok(other)),
        ("claude:opus high", Ok("ANSWER\nFINDING api/Endpoint.cs:2-3 the key is checked in Handle\nCONFIDENCE understanding=high result=high")),
        ("claude:opus high", Ok("Handle checks the key [1].")),
    ];
    let f = Fake::new(&script);
    let q = ["Why", "is", "the", "key", "checked?", "Should", "a", "blank", "key", "be", "rejected?"];
    let mut argv = q.to_vec();
    argv.extend(["--adaptive", "--agents", "qwen-a,claude:sonnet@high", "--prose"]);
    argv.extend(["--orchestrator", "claude:opus", "--orchestrator-effort", "high"]);
    let out = go(&root, &f, &argv).expect("run");
    assert!(out.contains("orchestrator: claude:opus high\nladder: 1 qwen-a low"), "{out}");
    assert!(out.contains("plan: 2 parts by claude:opus high\n"), "{out}");
    assert!(out.contains("answer (combined by claude:opus high):"), "{out}");
    assert!(out.contains("summary (by claude:opus high):\nHandle checks the key [1]."), "{out}");
    assert!(f.prompts("claude:sonnet high").is_empty(), "the ladder did not plan");
    assert!(out.contains("orchestration:\n{\n  \"orchestrator\": \"claude:opus high\""), "{out}");

    let records = f.records.lock().expect("lock").clone();
    assert_eq!(records.len(), 1);
    let r = &records[0];
    assert_eq!(
        (r["orchestrator"].as_str(), r["plan"].as_str(), r["combine"].as_str()),
        (Some("claude:opus high"), Some("planned"), Some("combined"))
    );
    assert_eq!(
        (r["v"].as_u64(), r["difficulty"].as_u64(), r["parts"].as_array().map(Vec::len)),
        (Some(1), Some(4), Some(2))
    );
    assert_eq!(r["calls"].as_array().map(Vec::len), Some(5));
    assert_eq!(r["calls"][0]["orchestrator"], true);
    assert_eq!(r["calls"][1]["orchestrator"], false);
    assert_eq!(r["usage"]["orchestrator"]["output_tokens"], 33);
    assert_eq!(r["parts"][0]["attempts"][0]["findings"], 1);
    assert_eq!(r["question_hash"].as_str().map(str::len), Some(12));
    assert!(!r.to_string().contains("blank key"), "no question text is logged");

    let f = Fake::new(&script);
    argv.push("--json");
    let v: Value = serde_json::from_str(&go(&root, &f, &argv).expect("json")).expect("parse");
    assert_eq!(v["orchestrator"], "claude:opus high");
    let o = &v["orchestration"];
    assert_eq!(o["orchestrator"], "claude:opus high");
    let steps: Vec<(&str, &str, &str)> = o["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|x| {
            (
                x["step"].as_str().unwrap_or_default(),
                x["by"].as_str().unwrap_or_default(),
                x["agent"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            ("planner", "orchestrator", "claude:opus"),
            ("part P1", "delegated", "qwen-a"),
            ("part P2", "delegated", "qwen-a"),
            ("combine", "orchestrator", "claude:opus"),
            ("summary", "orchestrator", "claude:opus"),
        ]
    );
    assert_eq!(
        (
            o["steps"][0]["input_tokens"].as_u64(),
            o["steps"][0]["output_tokens"].as_u64(),
            o["steps"][0]["input_estimated"].as_bool()
        ),
        (Some(100), Some(11), Some(false))
    );
    assert_eq!(
        (o["steps"][1]["input_estimated"].as_bool(), o["steps"][1]["output_tokens"].as_u64()),
        (Some(true), Some(7))
    );
    assert_eq!(
        (
            o["totals"]["orchestrator"]["output_tokens"].as_u64(),
            o["totals"]["delegated"]["output_tokens"].as_u64(),
            o["totals"]["orchestrator"]["steps"].as_u64()
        ),
        (Some(33), Some(14), Some(3))
    );

    let f = Fake::new(&[("claude:opus high", Err("down")), ("qwen-a low", Ok(KEY))]);
    let out = go(
        &root,
        &f,
        &["--adaptive", "--agents", "qwen-a,claude:sonnet@high", "--orchestrator", "claude:opus@high"]
            .iter()
            .chain(&q)
            .copied()
            .collect::<Vec<_>>(),
    )
    .expect("fallback");
    assert!(out.contains("planner claude:opus high failed: down\n"), "{out}");
    assert!(out.contains("plan: one part on qwen-a low (no planner answered)"), "{out}");
    assert_eq!(f.records.lock().expect("lock")[0]["plan"], "fallback");
    let bad = ["--adaptive", "--orchestrator", "claude:opus", "--orchestrator-effort", "huge", "q"];
    assert!(go(&root, &Fake::default(), &bad).unwrap_err().contains("takes effort"));
}

// Checks the orchestration log's location and privacy, its rotation and that an unwritable log is dropped quietly.
#[test]
fn test_orchestration_log() {
    let _g = crate::testutil::env_lock();
    let t = TempDir::new();
    let (saved_log, saved_home) = (std::env::var_os("ZAZZLE_ORCHESTRATION_LOG"), std::env::var_os("HOME"));
    std::env::set_var("ZAZZLE_ORCHESTRATION_LOG", "");
    assert_eq!(record::path(), None);
    record::write(&json!({"v": 1}));
    std::env::set_var("ZAZZLE_ORCHESTRATION_LOG", t.path().join("o.jsonl"));
    record::write(&json!({"v": 1}));
    assert_eq!(std::fs::read_to_string(t.path().join("o.jsonl")).expect("written").trim(), "{\"v\":1}");
    std::env::remove_var("ZAZZLE_ORCHESTRATION_LOG");
    assert_eq!(record::path(), None, "tests never write to the real home");
    assert_eq!(
        record::locate(None, Some(t.path().into())),
        Some(t.path().join(".zazzle/orchestration.jsonl"))
    );
    match saved_log {
        Some(v) => std::env::set_var("ZAZZLE_ORCHESTRATION_LOG", v),
        None => std::env::remove_var("ZAZZLE_ORCHESTRATION_LOG"),
    }
    match saved_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    let full = t.path().join("full.jsonl");
    std::fs::File::create(&full).expect("create").set_len(21 * 1024 * 1024).expect("grow");
    record::append(&full, &json!({"v": 1})).expect("append");
    assert!(full.with_extension("jsonl.1").exists() && std::fs::metadata(&full).expect("new").len() < 64);
    let blocked = t.write("file", b"x").join("o.jsonl");
    assert!(record::append(&blocked, &json!({})).is_err());
}

// Checks the planner's failures: an error, then a planner with no parts moves up; a last planner without parts leaves one part on the cheapest rung; a kept plan notes its doubt; no answering planner at all.
#[test]
fn test_planner_failures() {
    let (_t, root) = repo();
    let argv = [
        "Why",
        "is",
        "the",
        "key",
        "checked?",
        "Where",
        "is",
        "it",
        "stored?",
        "--path",
        "api",
        "--symbol",
        "Handle",
        "--adaptive",
        "--agents",
        "qwen-a,qwen-b,qwen-c",
    ];
    let f = Fake::new(&[
        ("qwen-b high", Err("busy")),
        ("qwen-c low", Ok("no plan here")),
        ("qwen-c low", Ok("PART 1 | I1 |")),
        ("qwen-a low", Ok(KEY)),
    ]);
    let out = go(&root, &f, &argv).expect("run");
    assert!(out.contains("planner qwen-b high failed: busy\nplanner qwen-c low's plan was refused: the reply had no PART lines\nplanner qwen-c low's repaired plan was refused too (I2 is in no part); the task runs as one assignment on qwen-a low\n"), "{out}");
    assert!(
        f.prompts("qwen-c low")[1]
            .contains("Your plan was refused: the reply had no PART lines. Reply again"),
        "one repair, told why"
    );
    assert!(out.contains("  P1 qwen-a low: complete"), "{out}");
    assert!(out.contains("answer:\n"), "one part needs no combining: {out}");

    let f = Fake::new(&[
        ("qwen-b low", Ok("PART 1 | I1, I2 |\nESCALATE I do not know the code")),
        ("qwen-b high", Ok("PART 1 | I1, I2 |\nCONFIDENCE understanding=low result=low")),
        ("qwen-a low", Ok(KEY)),
    ]);
    let out = go(
        &root,
        &f,
        &[
            "Why",
            "is",
            "the",
            "key",
            "checked?",
            "Where",
            "is",
            "it",
            "stored?",
            "--path",
            "api",
            "--symbol",
            "Handle",
            "--adaptive",
            "--agents",
            "qwen-a@low,qwen-a@medium,qwen-a@high,qwen-b",
        ],
    )
    .expect("run");
    assert!(
        out.contains(
            "planner qwen-b low escalated: it asked for a stronger planner: I do not know the code\n"
        ),
        "{out}"
    );
    assert!(out.contains("plan: 1 parts by qwen-b high, kept although it rated its understanding low\n  P1 qwen-a low: I1: Why is the key checked?\nI2: Where is it stored?\n"), "{out}");

    let f = Fake::new(&[("qwen-a low", Ok(KEY))]);
    let out = go(
        &root,
        &f,
        &[
            "Why",
            "is",
            "the",
            "key",
            "checked?",
            "Where",
            "is",
            "it",
            "stored?",
            "--path",
            "api",
            "--symbol",
            "Handle",
            "--adaptive",
            "--agents",
            "qwen-a@low,qwen-b@low",
        ],
    )
    .expect("run");
    assert!(out.contains("planner qwen-b low failed: no reply scripted for qwen-b low\nplan: one part on qwen-a low (no planner answered)\n"), "{out}");
}

// Checks escalation stops at the top of the ladder, after the most escalations, and when the time is out, and that a failing local model and an unusable hosted reply both escalate.
#[test]
fn test_part_escalation_limits() {
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
    let plan = investigate::parse(&["q".to_string()]).expect("plan");
    let scope = vec![root.clone()];
    let ladder = agent::ladder(&[
        "a@low".into(),
        "claude:b@low".into(),
        "c@low".into(),
        "d@low".into(),
        "e@low".into(),
    ])
    .expect("ladder");
    let f = Fake::new(&[
        ("a low", Err("the model is not downloaded")),
        ("claude:b low", Ok("I think it is fine.")),
        ("c low", Ok("ANSWER\nFINDING api/Endpoint.cs:99 nowhere")),
        ("d low", Ok("ANSWER\nUNKNOWN everything")),
    ]);
    let (log, p) = (Log::default(), Part { question: "q".into(), symbols: vec![], rung: 0 });
    let far = Instant::now() + Duration::from_secs(60);
    let a = part(&ctx, &f, &log, &plan, &scope, &ladder, &p, 1, far);
    let whys: Vec<String> = a.iter().map(|x| x.why.clone().unwrap_or_default()).collect();
    assert!(
        whys[0].starts_with("it failed: local model") || whys[0].starts_with("it failed: the model"),
        "{whys:?}"
    );
    assert!(whys[1].starts_with("it failed: the reply had none of the required lines"), "{whys:?}");
    assert_eq!(whys[2], "none of its citations checked out");
    assert_eq!(whys[3], "it verified nothing; not escalated: the part was escalated as often as allowed");
    assert!(
        story(&a).contains("a low (it failed")
            && story(&a).ends_with("not escalated: the part was escalated as often as allowed")
    );
    assert_eq!(
        log.calls().iter().map(|c| c.role.as_str()).collect::<Vec<_>>(),
        ["part P1", "escalation P1", "escalation P1", "escalation P1"]
    );

    let f = Fake::new(&[("e low", Ok("ANSWER\nUNKNOWN everything"))]);
    let top = Part { rung: 4, ..p.clone() };
    let a = part(&ctx, &f, &Log::default(), &plan, &scope, &ladder, &top, 1, far);
    assert_eq!(a[0].why.as_deref(), Some("it verified nothing; not escalated: it was the top of the ladder"));
    let f = Fake::new(&[("a low", Ok("ANSWER\nUNKNOWN everything"))]);
    let late = Log::default();
    let a = part(&ctx, &f, &late, &plan, &scope, &ladder, &p, 1, Instant::now());
    assert_eq!(
        a[0].why.as_deref(),
        Some("it failed: the time limit ran out before it started; not escalated: the time limit ran out")
    );
    assert!(late.calls().is_empty(), "no agent is asked after the deadline");
}

// Checks combining falls back to merging the parts when the combiner fails or verifies nothing from them, and how merging and overlap work.
#[test]
fn test_combine_fallbacks() {
    let (_t, root) = repo();
    let plan = investigate::parse(&["q".to_string()]).expect("plan");
    let scope = vec![root.clone()];
    let found = |c: &str| investigate::Found {
        claim: "c".into(),
        path: "api/Endpoint.cs".into(),
        cite: c.into(),
        excerpt: String::new(),
    };
    let a = Work {
        status: "complete".into(),
        understanding: "high".into(),
        confidence: "medium".into(),
        findings: vec![found("api/Endpoint.cs:2-3")],
        turns: 1,
        ..Work::default()
    };
    let b = Work {
        status: "partial".into(),
        confidence: "low".into(),
        findings: vec![found("api/Endpoint.cs:2-3")],
        gaps: vec!["g".into()],
        blocked: Some("x".into()),
        escalate: Some("e".into()),
        turns: 2,
        ..Work::default()
    };
    let m = merge(&[a.clone(), b.clone()]);
    assert_eq!(
        (m.status.as_str(), m.findings.len(), m.understanding.as_str(), m.confidence.as_str(), m.turns),
        ("partial", 1, "high", "low", 3)
    );
    assert_eq!(
        (m.blocked.as_deref(), m.escalate.as_deref(), m.gaps.clone()),
        (Some("x"), Some("e"), vec!["g".to_string()])
    );
    assert_eq!(merge(&[Work::failed("no")]).status, "failed");
    assert!(overlaps("a:2-3", "a:3") && !overlaps("a:2-3", "a:4-9") && !overlaps("a", "a:1"));
    let r = Rung::parse("qwen-a", None).expect("rung");
    let f = Fake::new(&[
        ("qwen-a low", Err("gone")),
        ("qwen-a low", Ok("ANSWER\nFINDING sim/Fake.cs:2 elsewhere")),
    ]);
    let log = Log::default();
    let far = Instant::now() + Duration::from_secs(60);
    let (w, by) = combine(&f, &log, &plan, &root, &scope, &[a.clone(), b.clone()], &r, far);
    assert_eq!(
        (w.findings.len(), by),
        (1, " (the parts merged; combining with qwen-a low failed: gone)".to_string())
    );
    let (w, by) = combine(&f, &log, &plan, &root, &scope, &[a, b], &r, far);
    assert_eq!(
        (w.status.as_str(), by.as_str()),
        ("partial", " (the parts merged; qwen-a low verified nothing from them)")
    );
    let s = summary(&f, &log, &plan, &root, &r, &w, far);
    assert_eq!(s, "summary: qwen-a low failed: no reply scripted for qwen-a low\n");
}

// Checks the plain run with a hosted benchmark (run alongside) and a review of both, as text and JSON.
#[test]
fn test_review_and_hosted_benchmark() {
    let (_t, root) = repo();
    let script = [
        ("local default low", Ok(KEY)),
        ("claude:opus medium", Ok("STATUS complete\nFINDING api/Endpoint.cs:2-3 the key is checked\nFINDING api/Endpoint.cs:40 beyond the file\nCONFIDENCE understanding=high result=high")),
        ("codex:gpt high", Ok("QUALITY A good cites the check\nQUALITY B fair one bad citation\nFINDING A1 supported matches line 3\nFINDING B1 unclear\nMISSING A nothing\nCOMPARE quality A no bad citation\nCOMPARE time b fewer seconds\nBETTER A more precise\nCONFIDENCE understanding=high result=medium")),
    ];
    let f = Fake::new(&script);
    let argv = [
        "Where",
        "is",
        "the",
        "key",
        "checked?",
        "--path",
        "api",
        "--benchmark-model",
        "claude:opus",
        "--review-model",
        "codex:gpt",
        "--review-effort",
        "high",
    ];
    let out = go(&root, &f, &argv).expect("run");
    assert!(
        out.starts_with("investigate: Where is the key checked?\nscope: api\nfindings (1 verified):\n"),
        "the plain report comes first: {out}"
    );
    assert!(out.contains("usage: 1 calls (investigate 1), "), "{out}");
    assert!(out.contains("benchmark: zazzle against claude:opus medium alone\n"), "{out}");
    assert!(
        out.contains(
            "  verified       1                      1\n  rejected       0                      1\n"
        ),
        "{out}"
    );
    assert!(out.contains("  cited files in common: 1 of 1\n"), "{out}");
    assert!(
        out.contains("usage (claude:opus medium): 1 calls (benchmark 1), ") && out.contains("$0.0100"),
        "{out}"
    );
    assert!(out.contains("review (by codex:gpt high):\n- A quality good: cites the check\n- B quality fair: one bad citation\n- finding A1 supported: matches line 3\n- finding B1 unclear: \n- A misses: nothing\n- quality: A: no bad citation\n- time: B: fewer seconds\n- not compared by the reviewer: tokens\n- better: A: more precise\n"), "{out}");
    assert!(
        out.contains("  wall seconds ")
            && out.contains("  input tokens   0                      100\n")
            && out.contains("  cached tokens  0                      40\n"),
        "{out}"
    );
    let rp = &f.prompts("codex:gpt high")[0];
    assert!(
        rp.contains("You may read the files")
            && rp.contains("A1 api/Endpoint.cs:2-3")
            && rp.contains("Result B:\nB1 ")
            && rp.contains("COMPARE quality A|B|tie")
            && rp.contains("COMPARE time A|B|tie")
            && rp.contains("COMPARE tokens A|B|tie")
            && rp.contains("Measured by zazzle, not estimated:\nA (zazzle): ")
            && rp.contains("s wall clock, 1 calls, 0 input, 0 cached and 7 output tokens, cost not reported\nB (claude:opus medium): ")
            && rp.contains("100 input, 40 cached and 11 output tokens, cost $0.0100"),
        "{rp}"
    );
    assert!(
        f.prompts("claude:opus medium")[0].contains("Scope (stay inside): api"),
        "the benchmark answers alone"
    );

    let f = Fake::new(&script);
    let mut argv = argv.to_vec();
    argv.push("--json");
    let v: Value = serde_json::from_str(&go(&root, &f, &argv).expect("json")).expect("parse");
    assert_eq!(
        (v["question"].as_str(), v["status"].as_str()),
        (Some("Where is the key checked?"), Some("complete"))
    );
    assert_eq!(v["benchmark"]["agent"], "claude:opus medium");
    assert_eq!(v["benchmark"]["result"]["rejected"].as_array().map(Vec::len), Some(1));
    assert_eq!(v["benchmark"]["usage"]["cost_usd"], 0.01);
    assert_eq!(v["review"]["better"], json!(["A", "more precise"]));
    assert_eq!(v["review"]["compared"][1], json!(["time", "b", "fewer seconds"]));
    assert_eq!(v["review"]["not_compared"], json!(["tokens"]));
    assert_eq!(v["review"]["result_confidence"], "medium");
}

// Checks a local benchmark runs after the main run through the lookup loop, and a review that fails or has no lines says so.
#[test]
fn test_local_benchmark_and_bad_reviews() {
    let (_t, root) = repo();
    let f = Fake::new(&[
        ("local default low", Ok(KEY)),
        ("qwen-b high", Err("not downloaded")),
        ("qwen-c low", Ok("looks fine to me")),
    ]);
    let argv = [
        "Where?",
        "--path",
        "api",
        "--benchmark-model",
        "qwen-b",
        "--benchmark-effort",
        "high",
        "--review-model",
        "qwen-c",
    ];
    let out = go(&root, &f, &argv).expect("run");
    assert!(
        out.contains("benchmark: zazzle against qwen-b high alone\n  ")
            && out.contains("  state          complete               failed\n"),
        "{out}"
    );
    assert!(out.contains("review (by qwen-c low):\n- failed: the review had none of the required lines (it began: looks fine to me)\n"), "{out}");
    let f = Fake::new(&[("local default low", Ok(KEY))]);
    let out = go(&root, &f, &["Where?", "--review-model", "qwen-c"]).expect("run");
    assert!(out.contains("review (by qwen-c low):\n- failed: no reply scripted for qwen-c low\n"), "{out}");
    assert!(go(&root, &f, &["Where?", "--review-model", "huh:x"])
        .unwrap_err()
        .contains("unknown agent kind"));
    assert!(go(&root, &f, &["Where?", "--json", "--path", "nope"]).unwrap_err().contains("--path nope"));
    assert!(go(&root, &f, &["--json"]).unwrap_err().starts_with("usage: investigate"));
    let none = Fake { installed: vec!["qwen-a".into()], ..Fake::new(&[("qwen-a low", Ok(KEY))]) };
    let out =
        go(&root, &none, &["Where", "is", "Handle?", "--path", "api", "--symbol", "Handle", "--adaptive"])
            .expect("installed");
    assert!(out.contains("ladder: 1 qwen-a low, 2 qwen-a high\n"), "{out}");
    let out = go(
        &root,
        &Fake::new(&[("local default low", Ok(KEY))]),
        &["Where", "is", "Handle?", "--path", "api", "--symbol", "Handle", "--adaptive"],
    )
    .expect("default");
    assert!(out.contains("ladder: 1 local default low, 2 local default high\n"), "{out}");
}

// Checks the structured result: statuses from a reply, every trigger, the handoff, JSON, the rendering and the listing.
#[test]
fn test_work() {
    let (_t, root) = repo();
    let scope = vec![root.clone()];
    let w = Work::from_reply(&root, &scope, "STATUS complete\nGAP g");
    assert_eq!((w.status.as_str(), w.trigger().as_deref()), ("partial", Some("it verified nothing")));
    let w = Work::from_reply(&root, &scope, "FINDING api/Endpoint.cs:3 null throws\nCONFIDENCE low low");
    assert_eq!(
        (w.status.as_str(), w.trigger().as_deref()),
        ("complete", Some("it rated its understanding of the task low"))
    );
    assert_eq!(Work::from_reply(&root, &scope, "GAP all").status, "failed");
    let mut w = Work::from_reply(&root, &scope, "STATUS partial\nFINDING api/Endpoint.cs:3 null throws\nCONFIDENCE high medium\nESCALATE which version?\nCONTEXT the rule");
    assert_eq!((w.status.as_str(), w.trigger().as_deref()), ("partial", Some("it asked to escalate")));
    let r = w.render();
    assert!(
        r.contains("1. null throws\n   api/Endpoint.cs:3\n")
            && r.contains("confidence: understanding high, result medium\nescalate: which version?\n"),
        "{r}"
    );
    assert_eq!(
        w.handoff("x", "why"),
        ["verified: null throws (api/Endpoint.cs:3)", "x did not settle it: why", "the rule"]
    );
    assert_eq!(
        w.listing("A", 1),
        "A1 api/Endpoint.cs:3 null throws\n       3\t    if (r.Key == null) throw new Missing();\n"
    );
    w.escalate = None;
    assert_eq!(w.trigger(), None);
    let v = w.json();
    assert_eq!(
        (v["task_understanding"].as_str(), v["escalation"]["recommended"].as_bool()),
        (Some("high"), Some(false))
    );
    w.blocked = Some("access".into());
    assert!(w.render().contains("needs the user: access") && w.trigger().is_none());
    let none = Work::failed("boom");
    assert_eq!(
        (none.trigger().as_deref(), none.json()["task_understanding"].clone()),
        (Some("it failed: boom"), Value::Null)
    );
    assert!(none
        .render()
        .contains("(none)\ngaps:\n- boom\nconfidence: understanding not given, result not given\n"));
    assert_eq!(none.listing("A", 0), "(no verified findings)\nGAP boom\n");
    assert_eq!(none.summary(), "failed, 0 verified, understanding -, result -");
    assert_eq!(Work { status: "failed".into(), ..Work::default() }.trigger().as_deref(), Some("it failed"));
    let rejected = Work::from_reply(&root, &scope, "FINDING api/Endpoint.cs:99 x\nCONFIDENCE high high");
    assert_eq!(rejected.trigger().as_deref(), Some("it failed"), "a reply with nothing verified fails");
    let rej = Work { status: "partial".into(), rejected: vec!["r".into()], ..Work::default() };
    assert_eq!(rej.trigger().as_deref(), Some("none of its citations checked out"));
    assert_eq!(
        (work::lowest("", "low"), work::lowest("high", ""), work::lowest("medium", "low")),
        ("low".into(), "high".into(), "low".into())
    );
}

// Checks rung specs, labels and ladders, and reading installed models.
#[test]
fn test_rungs() {
    let r = |s: &str| Rung::parse(s, None);
    assert_eq!(r("claude:opus@max").expect("claude").label(), "claude:opus max");
    assert_eq!(r("codex:gpt-5.5").expect("codex").label(), "codex:gpt-5.5 medium");
    assert_eq!(r("local:qwen3.5-9b@high").expect("local").agent(), "qwen3.5-9b");
    assert!(r("claude:").unwrap_err().contains("names no model"));
    assert!(r("codex:x@max").unwrap_err().contains("takes effort minimal, low, medium, high, xhigh"));
    assert!(r("gpt:x").unwrap_err().contains("unknown agent kind"));
    assert!(!r("qwen").expect("q").hosted() && r("claude:x").expect("c").hosted());
    let l: Vec<String> = agent::ladder(&["a".into(), " ".into(), "claude:b@high".into()])
        .expect("ladder")
        .iter()
        .map(Rung::label)
        .collect();
    assert_eq!(l, ["a low", "a high", "claude:b high"]);
    let status = json!({"models": [
        {"name": "big", "bytes": 9, "check": "size"}, {"name": "small", "bytes": 2, "check": "sha256"},
        {"name": "bad", "bytes": 1, "check": "corrupt"}, {"name": "odd", "check": "size"}
    ]});
    assert_eq!(agent::installed_models(&status), ["small", "big"]);
    assert!(agent::installed_models(&Value::Null).is_empty());
}

// Checks reading the hosted agents' output: claude's JSON result and error, codex's event lines and last-message file.
#[test]
fn test_said() {
    use agent::{said, Kind};
    let c = said(
        Kind::Claude,
        r#"{"result":"STATUS complete","usage":{"output_tokens":42,"input_tokens":5,"cache_read_input_tokens":20},"num_turns":3,"total_cost_usd":0.5}"#,
        "",
    )
    .expect("claude");
    assert_eq!(
        c,
        Said {
            text: "STATUS complete".into(),
            tokens: 42,
            input: 25,
            cached: 20,
            rounds: 3,
            cost: Some(0.5),
            counted: true
        }
    );
    assert_eq!(
        said(Kind::Claude, r#"{"result":"rate limited","is_error":true}"#, ""),
        Err("claude failed: rate limited".into())
    );
    assert!(said(Kind::Claude, "not json", "").unwrap_err().contains("no JSON result"));
    let events = "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"from events\"}}\nnoise\n{\"type\":\"turn.completed\",\"usage\":{\"output_tokens\":9,\"input_tokens\":30,\"cached_input_tokens\":12}}";
    assert_eq!(
        said(Kind::Codex, events, "from file").expect("file"),
        Said {
            text: "from file".into(),
            tokens: 9,
            input: 30,
            cached: 12,
            rounds: 1,
            cost: None,
            counted: true
        }
    );
    assert_eq!(said(Kind::Codex, events, " ").expect("events").text, "from events");
    assert_eq!(said(Kind::Codex, "", ""), Err("codex gave no final message".into()));
    assert_eq!(agent::one_line(&"x ".repeat(150)).chars().count(), 201);
}

// Checks the real command lines through stand-in programs: claude's flags and prompt on standard input, codex's last-message file, a failing exit and the deadline stopping a slow agent.
#[test]
fn test_ask_hosted() {
    let t = TempDir::new();
    let exe = |name: &str, body: &str| {
        let p = t.write(name, format!("#!/bin/sh\n{body}").as_bytes());
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        p
    };
    let far = Instant::now() + Duration::from_secs(30);
    let claude = exe("claude", "echo \"$@\" > \"$0.args\"\ncat > \"$0.in\"\necho '{\"result\":\"STATUS complete\",\"usage\":{\"output_tokens\":3}}'\n");
    let r = Rung::parse("claude:sonnet@high", None).expect("rung");
    let s = agent::ask_hosted(&r, &claude, "the prompt", t.path(), far, false).expect("claude");
    assert_eq!((s.text.as_str(), s.tokens), ("STATUS complete", 3));
    let args = std::fs::read_to_string(t.path().join("claude.args")).expect("args");
    assert_eq!(args.trim(), "-p --model sonnet --effort high --permission-mode dontAsk --allowedTools Read,Grep,Glob --output-format json");
    assert_eq!(std::fs::read_to_string(t.path().join("claude.in")).expect("in"), "the prompt");
    let codex = exe("codex", "echo \"$@\" > \"$0.args\"\ncat > /dev/null\nfor a; do last=$prev; prev=$a; done\nprintf 'GAP from codex' > \"$last\"\n");
    let r = Rung::parse("codex:gpt@low", None).expect("rung");
    assert_eq!(
        agent::ask_hosted(&r, &codex, "p", t.path(), far, false).expect("codex").text,
        "GAP from codex"
    );
    let args = std::fs::read_to_string(t.path().join("codex.args")).expect("args");
    assert!(args.starts_with("exec --model gpt -c model_reasoning_effort=\"low\" --sandbox read-only --skip-git-repo-check --json --output-last-message ") && args.trim_end().ends_with(" -"), "{args}");
    let bad = exe("bad", "echo 'first' >&2\necho 'quota exceeded' >&2\nexit 3\n");
    let err = agent::ask_hosted(&r, &bad, "p", t.path(), far, true).unwrap_err();
    assert!(err.starts_with("codex:gpt low failed (") && err.ends_with(": quota exceeded"), "{err}");
    let slow = exe("slow", "sleep 5\n");
    let t0 = Instant::now();
    let err = agent::ask_hosted(&r, &slow, "p", t.path(), Instant::now() + Duration::from_millis(200), false)
        .unwrap_err();
    assert_eq!(err, "the time limit ran out while codex:gpt low worked");
    let stop = crate::budget::Stop::default();
    let canceller = {
        let s = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            s.cancel();
        })
    };
    let t1 = Instant::now();
    let err = crate::budget::stopping(&stop, || agent::ask_hosted(&r, &slow, "p", t.path(), far, false))
        .unwrap_err();
    canceller.join().expect("cancel");
    assert_eq!(err, "the run was cancelled while codex:gpt low worked");
    assert!(t1.elapsed() < Duration::from_secs(2), "the agent was stopped at the cancel: {:?}", t1.elapsed());
    assert!(t0.elapsed() < Duration::from_secs(4));
    let w = Rung::parse("claude:sonnet@low", None).expect("rung");
    agent::ask_hosted(&w, &claude, "edit", t.path(), far, true).expect("write mode");
    let args = std::fs::read_to_string(t.path().join("claude.args")).expect("args");
    assert!(args.contains("--allowedTools Read,Grep,Glob,Edit,Write"), "{args}");
    let wc = agent::command(&r, &codex, Path::new("o"), true);
    assert!(wc.get_args().any(|a| a == "workspace-write"), "codex writes in a workspace sandbox");
    assert!(agent::ask_hosted(&r, &t.path().join("missing"), "p", t.path(), far, false)
        .unwrap_err()
        .starts_with("starting "));
}

// Checks the real agents end to end through stand-ins found by environment variable: the installed models from ai-agent's status, its worker, and a hosted claude, through the dispatcher.
#[test]
fn test_real_agents_through_zazzle() {
    let _g = crate::testutil::env_lock();
    let (t, root) = repo();
    let worker = crate::testutil::fake_worker(
        &t,
        "worker",
        &[(
            "CONFIDENCE",
            "ANSWER\nFINDING api/Endpoint.cs:3 a null key throws\nCONFIDENCE understanding=high result=high",
        )],
    );
    let status = r#"{"models":[{"name":"qwen-a","bytes":1,"check":"size"}]}"#;
    let llm = t.write(
        "ai-agent",
        format!(
            "#!/bin/sh\nif [ \"$1\" = status ]; then echo '{status}'; exit 0; fi\nexec {} \"$@\"\n",
            worker.display()
        )
        .as_bytes(),
    );
    std::fs::set_permissions(&llm, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    let claude = t.write("claude", b"#!/bin/sh\ncat > /dev/null\necho '{\"result\":\"QUALITY A good fine\",\"usage\":{\"output_tokens\":2},\"total_cost_usd\":0.02}'\n");
    std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    std::env::set_var("ZAZZLE_LLM", &llm);
    std::env::set_var("ZAZZLE_CLAUDE", &claude);
    std::env::set_var("ZAZZLE_ORCHESTRATION_LOG", t.path().join("orchestration.jsonl"));
    let o = crate::testutil::run_cmd(
        &root,
        &[
            "investigate",
            "Where",
            "is",
            "Handle?",
            "--path",
            "api",
            "--symbol",
            "Handle",
            "--adaptive",
            "--review-model",
            "claude:opus",
        ],
    );
    std::env::remove_var("ZAZZLE_CLAUDE");
    std::env::remove_var("ZAZZLE_ORCHESTRATION_LOG");
    let logged = std::fs::read_to_string(t.path().join("orchestration.jsonl")).expect("the run was logged");
    assert_eq!(logged.lines().count(), 1, "{logged}");
    let _ =
        calls::Real.hosted(&Rung::parse("codex:x", None).expect("rung"), "p", &root, Instant::now(), false);
    std::env::set_var("ZAZZLE_LLM", t.path().join("none"));
    let empty = calls::Real.installed();
    std::env::remove_var("ZAZZLE_LLM");
    assert!(o.text.contains("ladder: 1 qwen-a low, 2 qwen-a high\n"), "{}", o.text);
    assert!(o.text.contains("  P1 qwen-a low: complete, 1 verified"), "{}", o.text);
    assert!(o.text.contains("review (by claude:opus medium):\n- A quality good: fine\n"), "{}", o.text);
    assert!(empty.is_empty());
}

// Checks the paths a run takes when agents fail: a hosted benchmark that errs, a local benchmark that answers within a given --seconds, a one-rung hosted ladder whose part fails with nothing higher, and a plain answer cut short that asks to escalate and needs the user.
#[test]
fn test_failing_agents() {
    let (_t, root) = repo();
    let f = Fake::new(&[("local default low", Ok(KEY)), ("qwen-b low", Ok(KEY))]);
    let out = go(&root, &f, &["Where?", "--path", "api", "--seconds", "30", "--benchmark-model", "qwen-b"])
        .expect("local bench");
    assert!(out.contains("  state          complete               complete\n"), "{out}");
    assert_eq!(f.prompts("qwen-b low").len(), 1, "the benchmark ran the lookup loop once");
    let f = Fake::new(&[("local default low", Ok(KEY)), ("claude:opus medium", Err("not logged in"))]);
    let out = go(&root, &f, &["Where?", "--path", "api", "--benchmark-model", "claude:opus"])
        .expect("hosted bench");
    assert!(
        out.contains("claude:opus medium alone:\nfindings (0 verified):\n(none)\ngaps:\n- not logged in\n"),
        "{out}"
    );
    let f = Fake::new(&[("claude:x low", Err("quota exceeded"))]);
    let argv = [
        "Where",
        "is",
        "Handle?",
        "--path",
        "api",
        "--symbol",
        "Handle",
        "--adaptive",
        "--agents",
        "claude:x@low",
    ];
    let out = go(&root, &f, &argv).expect("one rung");
    assert!(out.contains("  P1 claude:x low: failed, 0 verified, understanding -, result -; it failed: quota exceeded; not escalated: it was the top of the ladder\n"), "{out}");
    assert!(
        out.contains("-- adaptive: failed (it failed: quota exceeded; nothing higher settled it)."),
        "{out}"
    );
    assert!(
        f.prompts("claude:x low")[0].contains("Starting symbols: Handle\n"),
        "the hosted executor gets the symbols"
    );
    let cut = "ANSWER\nFINDING api/Endpoint.cs:2-3 Handle checks the key CUT\nESCALATE which version may enforce it?\nCONTEXT the frozen contract\nBLOCKED decision the release owner decides";
    let f = Fake::new(&[("local default low", Ok(cut)), ("qwen-c low", Ok("QUALITY A fair cut short"))]);
    let out = go(&root, &f, &["Where?", "--path", "api", "--review-model", "qwen-c"]).expect("cut");
    assert!(out.contains("escalate: which version may enforce it? (context: the frozen contract)\nneeds the user: decision the release owner decides\n"), "{out}");
    let f = Fake::new(&[("local default low", Ok(cut)), ("qwen-c low", Ok("QUALITY A fair cut short"))]);
    let argv = ["Where?", "--path", "api", "--review-model", "qwen-c", "--json"];
    let v: Value = serde_json::from_str(&go(&root, &f, &argv).expect("json")).expect("parse");
    assert_eq!(
        (v["status"].as_str(), v["needs_user"].as_str()),
        (Some("partial"), Some("decision the release owner decides"))
    );
    let f = Fake::new(&[
        ("local default low", Ok("ANSWER\nESCALATE who knows?")),
        ("qwen-c low", Ok("QUALITY A poor")),
    ]);
    assert!(go(&root, &f, &argv[..5]).expect("bare escalate").contains("escalate: who knows?\n"));
}

// Checks a spawn is retried while the program is busy, then given up, and other errors returned at once.
#[test]
fn test_retry_busy() {
    let mut n = 0;
    let r = agent::retry_busy(|| {
        n += 1;
        if n < 3 {
            Err(std::io::Error::from_raw_os_error(26))
        } else {
            Ok(n)
        }
    });
    assert_eq!(r.expect("third try"), 3);
    let mut m = 0;
    let busy: std::io::Result<()> = agent::retry_busy(|| {
        m += 1;
        Err(std::io::Error::from_raw_os_error(26))
    });
    assert!(busy.is_err() && m == 11);
    let other: std::io::Result<()> = agent::retry_busy(|| Err(std::io::Error::other("no")));
    assert_eq!(other.unwrap_err().to_string(), "no");
}

// Checks independent calls run at the same time and come back in order: the first waits for a message only the second sends, which one after another would never arrive.
#[test]
fn test_all_runs_together() {
    let (tx, rx) = std::sync::mpsc::channel::<usize>();
    let (tx, rx) = (Mutex::new(Some(tx)), Mutex::new(rx));
    let got = calls::all(&[0usize, 1, 2], |i, x| match i {
        0 => rx.lock().expect("rx").recv_timeout(Duration::from_secs(20)).map_or(99, |v| v + x),
        _ => {
            if let Some(t) = tx.lock().expect("tx").take() {
                t.send(40).expect("send");
            }
            x * 10
        }
    });
    assert_eq!(got, [40, 10, 20]);
    assert_eq!(calls::all(&[7], |i, x| i + x), [7]);
    assert!(calls::all(&[] as &[usize], |_, x| *x).is_empty());
}

// Checks one deadline bounds the whole job: a part still working at the deadline is stopped, its escalation and the combining are not started after it, the other part's result is kept, and the job ends at the deadline rather than after the extra time each step used to get.
#[test]
fn test_deadline_between_parts() {
    let (_t, root) = repo();
    let f = Fake::new(&[
        ("claude:sonnet high", Ok("PART 1 | I1 | Handle\nPART 3 | I2 |\nCONFIDENCE understanding=high result=high")),
        ("qwen-a low", Ok("SLEEP 5000\nANSWER\nFINDING api/Endpoint.cs:3 too late")),
        ("claude:sonnet high", Ok("STATUS complete\nFINDING api/Endpoint.cs:2-3 the key is checked in Handle\nCONFIDENCE understanding=high result=high")),
    ]);
    let q = ["Why", "is", "the", "key", "checked?", "Should", "a", "blank", "key", "be", "rejected?"];
    let mut argv = q.to_vec();
    argv.extend(["--adaptive", "--agents", "qwen-a,claude:sonnet@high", "--seconds", "2"]);
    let t0 = Instant::now();
    let out = go(&root, &f, &argv).expect("run");
    assert!(t0.elapsed() < Duration::from_millis(2300), "ended by the deadline: {:?}", t0.elapsed());
    assert!(
        out.contains("  P1 qwen-a low: failed")
            && out.contains("stopped at the time limit; not escalated: the time limit ran out"),
        "{out}"
    );
    assert!(
        out.contains(
            "combining with claude:sonnet high failed: the time limit ran out before combine could start"
        ),
        "{out}"
    );
    assert!(out.contains("the key is checked in Handle"), "the other part's result is kept: {out}");
    assert!(f.prompts("qwen-a high").is_empty(), "no escalation after the deadline");
    assert_eq!(
        f.prompts("claude:sonnet high").len(),
        2,
        "planner and P2, no combining after the deadline: {out}"
    );
}

// Checks no model call starts once the deadline has passed, local or hosted, and none is recorded.
#[test]
fn test_no_call_after_the_deadline() {
    let (f, log) = (Fake::new(&[("a low", Ok("x")), ("claude:b low", Ok("y"))]), Log::default());
    let past = Instant::now();
    let local = Rung::parse("a", Some("low")).expect("rung");
    let hosted = Rung::parse("claude:b", Some("low")).expect("rung");
    let e = log.local(&f, &local, "part P1", &serde_json::json!({"user": "q"}), &[], past).unwrap_err();
    assert_eq!(e, "the time limit ran out before part P1 could start");
    assert!(log
        .hosted(&f, &hosted, &Ask::new("combine", "", "q", &[]), Path::new("."), past)
        .unwrap_err()
        .contains("before combine could start"));
    assert!(log.calls().is_empty() && f.prompts("a low").is_empty() && f.prompts("claude:b low").is_empty());
}

// Checks a cancel reaches every part running at the same time: each stops at once, nothing new starts after it (no escalation, no combining), and the run ends quickly with the cancel as the reason.
#[test]
fn test_cancel_reaches_parallel_parts() {
    let (_t, root) = repo();
    let f = Fake::new(&[
        (
            "claude:sonnet high",
            Ok("PART 1 | I1 | Handle\nPART 3 | I2 |\nCONFIDENCE understanding=high result=high"),
        ),
        ("qwen-a low", Ok("SLEEP 5000\nANSWER\nFINDING api/Endpoint.cs:3 too late")),
        ("claude:sonnet high", Ok("SLEEP 5000\nSTATUS complete\nFINDING api/Endpoint.cs:2-3 too late")),
    ]);
    let q = ["Why", "is", "the", "key", "checked?", "Should", "a", "blank", "key", "be", "rejected?"];
    let mut argv = q.to_vec();
    argv.extend(["--adaptive", "--agents", "qwen-a,claude:sonnet@high", "--seconds", "60"]);
    let stop = crate::budget::Stop::default();
    let canceller = {
        let s = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            s.cancel();
        })
    };
    let t0 = Instant::now();
    let out = crate::budget::stopping(&stop, || go(&root, &f, &argv)).expect("run");
    canceller.join().expect("cancel");
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    assert!(
        out.contains("not escalated: the run was cancelled") && out.contains("stopped at the cancel"),
        "{out}"
    );
    assert!(out.contains("the run was cancelled before combine could start"), "{out}");
    assert!(f.prompts("qwen-a high").is_empty());
}

// Checks a benchmark gets a judge by default: Claude Opus at high effort when `claude` is installed, comparing quality, time and tokens, and said to be zazzle's pick; else the strongest hosted rung it can run at high effort, else the strongest local rung; --no-review leaves it out, and asking for both is refused.
#[test]
fn test_benchmark_picks_a_judge() {
    let (_t, root) = repo();
    let judged = "QUALITY A good cites the check\nQUALITY B good also\nCOMPARE quality tie same finding\nCOMPARE time A fewer seconds\nCOMPARE tokens A no hosted tokens\nBETTER A cheaper for the same answer\nCONFIDENCE understanding=high result=high";
    let script = |judge: &str| {
        vec![
            ("local default low".to_string(), Ok(KEY.to_string())),
            ("claude:sonnet medium".to_string(), Ok("STATUS complete\nFINDING api/Endpoint.cs:2-3 the key is checked\nCONFIDENCE understanding=high result=high".to_string())),
            (judge.to_string(), Ok(judged.to_string())),
        ]
    };
    let fake = |judge: &str, hosts: Vec<agent::Kind>| {
        let s = script(judge);
        let pairs: Vec<(&str, Result<&str, &str>)> =
            s.iter().map(|(w, r)| (w.as_str(), r.as_deref().map_err(|e: &String| e.as_str()))).collect();
        Fake { hosts, ..Fake::new(&pairs) }
    };
    let argv =
        ["Where", "is", "the", "key", "checked?", "--path", "api", "--benchmark-model", "claude:sonnet"];
    let f = fake("claude:opus high", vec![agent::Kind::Claude]);
    let out = go(&root, &f, &argv).expect("run");
    assert!(
        out.contains(
            "review (by claude:opus high, the default judge for a benchmark: the strongest agent available"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "- quality: TIE: same finding\n- time: A: fewer seconds\n- tokens: A: no hosted tokens\n"
        ) && !out.contains("not compared"),
        "{out}"
    );
    assert!(
        f.prompts("claude:opus high")[0].contains("Measured by zazzle"),
        "the judge gets the measurements"
    );
    let mut json_argv = argv.to_vec();
    json_argv.push("--json");
    let f = fake("claude:opus high", vec![agent::Kind::Claude]);
    let v: Value = serde_json::from_str(&go(&root, &f, &json_argv).expect("json")).expect("parse");
    assert_eq!(
        (v["review"]["agent"].as_str(), v["review"]["picked_by_default"].as_bool()),
        (Some("claude:opus high"), Some(true))
    );

    let f = fake("local default high", vec![]);
    let out = go(&root, &f, &argv).expect("no hosted");
    assert!(
        out.contains("review (by local default high, the default judge"),
        "no hosted agent: the top of the ladder judges: {out}"
    );

    let mut off = argv.to_vec();
    off.push("--no-review");
    let f = fake("claude:opus high", vec![agent::Kind::Claude]);
    let out = go(&root, &f, &off).expect("off");
    assert!(!out.contains("review (by") && f.prompts("claude:opus high").is_empty(), "{out}");
    off.extend(["--review-model", "codex:gpt"]);
    assert!(go(&root, &f, &off).unwrap_err().contains("--no-review and --review-model"));

    let ladder = agent::ladder(&["qwen-a".into(), "codex:gpt@low".into()]).expect("ladder");
    let f = Fake { hosts: vec![agent::Kind::Codex], ..Fake::default() };
    assert_eq!(
        default_judge(&f, &ladder).label(),
        "codex:gpt high",
        "the strongest hosted rung, at high effort"
    );
    let f = Fake::default();
    assert_eq!(
        default_judge(&f, &ladder).label(),
        "qwen-a high",
        "a hosted rung the machine cannot run is skipped: the strongest local rung"
    );
    assert_eq!(default_judge(&f, &[]).label(), "local default high");
}

// Checks a hosted part of a question with no starting symbol is told the declarations zazzle found for it, so it starts from real code, while the benchmark's agent alone is not.
#[test]
fn test_hosted_parts_get_discovered_leads() {
    let (_t, root) = repo();
    let f = Fake::new(&[
        ("claude:sonnet high", Ok("STATUS complete\nFINDING api/Endpoint.cs:2-3 the key is checked in Handle\nCONFIDENCE understanding=high result=high")),
        ("claude:opus medium", Ok("STATUS complete\nCONFIDENCE understanding=high result=high")),
    ]);
    let argv = [
        "Where",
        "does",
        "the",
        "endpoint",
        "handle",
        "a",
        "request?",
        "--adaptive",
        "--agents",
        "claude:sonnet@high",
        "--benchmark-model",
        "claude:opus",
        "--no-review",
    ];
    go(&root, &f, &argv).expect("run");
    let asked = &f.prompts("claude:sonnet high")[0];
    assert!(
        asked.contains("Declarations zazzle found in the files that match the question")
            && asked.contains("api/Endpoint.cs:2-5 method Handle"),
        "{asked}"
    );
    assert!(
        !f.prompts("claude:opus medium")[0].contains("Declarations zazzle found"),
        "the agent alone gets no help from zazzle"
    );
}

// Checks a plan with numeric or model-label questions, an agent off the ladder or a reply cut short is never dispatched: the planner gets one repair, a local planner is held to the plan grammar, and when the repair is refused too the task runs as one assignment with its own words.
#[test]
fn test_invalid_plans_dispatch_nothing() {
    let (_t, root) = repo();
    let q = [
        "Why",
        "is",
        "the",
        "key",
        "checked?",
        "Where",
        "is",
        "the",
        "request",
        "stored?",
        "--adaptive",
        "--agents",
        "qwen-a,qwen-b,qwen-c",
    ];
    let f = Fake::new(&[
        (
            "qwen-c high",
            Ok("PART 1 | 1 |\nPART 1 | qwen-a low | Where?\nCONFIDENCE understanding=high result=high"),
        ),
        ("qwen-c high", Ok("PART 9 | I1, I2 |\nCUT")),
        ("qwen-a low", Ok(KEY)),
    ]);
    let out = go(&root, &f, &q).expect("run");
    assert!(out.contains("planner qwen-c high's plan was refused: `PART 1 | 1 |`: `1` is not an item of the task; `PART 1 | qwen-a low | Where?`: `qwen-a low` is not an item of the task"), "{out}");
    assert!(out.contains("planner qwen-c high's repaired plan was refused too (the plan was cut short; `PART 9 | I1, I2 |`: agent 9 is not on the ladder (1 to 6)"), "{out}");
    assert!(out.contains("the task runs as one assignment on qwen-a low"), "{out}");
    let planned = f.prompts("qwen-c high");
    assert!(
        planned.iter().all(|p| p.ends_with("[held to a grammar]")),
        "a local planner is held to the plan grammar"
    );
    let asked = f.prompts("qwen-a low");
    assert_eq!(asked.len(), 1, "one executor call, nothing invalid dispatched");
    assert!(
        asked[0].contains("Question: Why is the key checked? Where is the request stored?"),
        "the original task, in its own words: {}",
        asked[0]
    );
    assert!(!asked.iter().any(|p| p.contains("Question: 1\n") || p.contains("Question: qwen-a low")));
}

// Checks several questions about one component (the cache) run as one investigation with no planner and no combiner call.
#[test]
fn test_one_component_needs_no_planner() {
    let (t, root) = repo();
    t.write("api/StationCache.cs", b"class StationCache {\n  string Key(string station, int bay, string terminal) => $\"{station}:{bay}:{terminal}\";\n  bool Recall(string question) => question == stored;\n}\n");
    let f = Fake::new(&[("qwen-a low", Ok("ANSWER\nFINDING api/StationCache.cs:2 the key holds station, bay and terminal\nCONFIDENCE understanding=high result=high"))]);
    let q = [
        "What",
        "is",
        "the",
        "station",
        "cache",
        "key?",
        "When",
        "does",
        "the",
        "station",
        "cache",
        "compare",
        "the",
        "stored",
        "question?",
        "--adaptive",
        "--agents",
        "qwen-a,claude:opus@high",
    ];
    let out = go(&root, &f, &q).expect("run");
    assert!(
        out.contains("plan: one part on qwen-a low, no planner call (every item is about StationCache)\n"),
        "{out}"
    );
    assert!(f.prompts("claude:opus high").is_empty(), "no planner and no combiner");
    assert!(out.contains("answer:\n") && !out.contains("combined by"), "{out}");
}
