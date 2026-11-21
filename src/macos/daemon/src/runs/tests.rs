// Tests for the run store: results and usage kept and returned for the same id, refusals, a waiter woken when the run ends, and runs interrupted by a daemon restart or a panic.
use super::*;
use crate::testutil::TempDir;

// A command line as owned strings.
fn line(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// Starts a run that must be new.
fn new_run(root: &Path, id: &str, argv: &[String]) -> Job {
    match begin(root, id, argv, root, Instant::now() + Duration::from_secs(30)).expect("begin") {
        Begun::New(j) => j,
        Begun::Earlier(r) => panic!("not new: {r:?}"),
    }
}

// Looks a run up and returns what it holds.
fn earlier(root: &Path, id: &str, argv: &[String]) -> Result<Output, String> {
    match begin(root, id, argv, root, Instant::now() + Duration::from_secs(30))? {
        Begun::Earlier(r) => r,
        Begun::New(_) => panic!("started again"),
    }
}

// A usage record: a local call that answered with 5 tokens, or a hosted one stopped before it reported any counts.
fn call(hosted: bool) -> Value {
    match hosted {
        false => Call {
            role: "part P1".into(),
            rung: "qwen low".into(),
            tokens: Some(5),
            ok: true,
            secs: 1.0,
            ..Call::default()
        },
        true => Call {
            role: "part P2".into(),
            rung: "claude:s high".into(),
            hosted: true,
            secs: 2.0,
            ..Call::default()
        },
    }
    .json()
}

// Checks a finished run's result and usage come back for the same id or the id alone, without starting again, and the refusals.
#[test]
fn test_runs_kept_and_reused() {
    let t = TempDir::new();
    let root = t.path().join("runs");
    let argv = line(&["q", "--seconds", "5"]);
    let job = new_run(&root, "t1", &argv);
    append(&job.usage(), &call(false)).expect("usage");
    job.finish(&Ok(Output::plain("the answer".into()))).expect("finish");
    assert_eq!(earlier(&root, "t1", &argv).expect("same").text, "the answer");
    assert_eq!(earlier(&root, "t1", &[]).expect("alone").text, "the answer");
    assert!(earlier(&root, "t1", &line(&["other"])).unwrap_err().contains("another command line"));
    assert_eq!(earlier(&root, "nope", &[]).unwrap_err(), "no investigate run nope");
    assert!(begin(&root, "../x", &argv, &root, Instant::now())
        .err()
        .expect("bad id")
        .contains("--run-id takes"));
    let kept: Value = serde_json::from_str(&fs::read_to_string(root.join("t1/result.json")).expect("result"))
        .expect("json");
    assert_eq!(
        (kept["state"].as_str(), kept["usage"]["calls"].as_u64(), kept["usage"]["output_tokens"].as_u64()),
        (Some("done"), Some(1), Some(5))
    );
    let mode = |p: &Path| {
        std::os::unix::fs::PermissionsExt::mode(&fs::metadata(p).expect("meta").permissions()) & 0o777
    };
    assert_eq!(
        (mode(&root.join("t1")), mode(&root.join("t1/result.json")), mode(&root.join("t1/usage.jsonl"))),
        (0o700, 0o600, 0o600)
    );
    let job = new_run(&root, "t2", &argv);
    job.finish(&Err("bad flag".into())).expect("finish");
    assert_eq!(earlier(&root, "t2", &[]), Err("bad flag".into()));
    assert!(is_id(&new_id()) && !is_id(".hidden") && !is_id(&"x".repeat(65)) && !is_id(""));
}

// Checks a second submission waits for the run working here and gets its result when it ends, a run left by a stopped daemon or a panic reads as interrupted with the usage it recorded (unreported counts unknown, not zero), a run past its deadline is reported, and old runs are reaped.
#[test]
fn test_runs_wait_and_interrupt() {
    let t = TempDir::new();
    let root = t.path().join("runs");
    let argv = line(&["q"]);
    let job = new_run(&root, "w1", &argv);
    let t0 = Instant::now();
    let done = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        job.finish(&Ok(Output::plain("late answer".into()))).expect("finish");
    });
    assert_eq!(earlier(&root, "w1", &argv).expect("waited").text, "late answer");
    assert!(t0.elapsed() >= Duration::from_millis(150));
    done.join().expect("finisher");

    let gone = root.join("r1");
    fs::create_dir_all(&gone).expect("folder");
    fs::write(
        gone.join(REQUEST),
        json!({"argv": ["q"], "cwd": root, "snapshot": snapshot(&root)}).to_string(),
    )
    .expect("request");
    append(&gone.join(USAGE), &call(false)).expect("local");
    append(&gone.join(USAGE), &call(true)).expect("hosted");
    let o = earlier(&root, "r1", &argv).expect("interrupted");
    assert_eq!(o.status, 1);
    assert!(
        o.text.contains("run r1 was interrupted")
            && o.text.contains(
                "2 calls (part 2), 3.0 s, 5 output tokens (+ unknown for 1 call(s) that reported no counts)"
            ),
        "{}",
        o.text
    );

    let job = new_run(&root, "p1", &argv);
    drop(job);
    assert!(earlier(&root, "p1", &[]).expect("panicked").text.contains("run p1 was interrupted"));

    match begin(&root, "s1", &argv, &root, Instant::now()).expect("begin") {
        Begun::New(stuck) => {
            assert_eq!(
                earlier(&root, "s1", &argv),
                Err("run s1 is still working past its time limit".into())
            );
            drop(stuck);
        }
        Begun::Earlier(_) => panic!("new"),
    }

    let old = root.join("old");
    fs::create_dir_all(&old).expect("old");
    let long_ago = SystemTime::now() - MAX_AGE - Duration::from_secs(60);
    fs::File::open(&old).expect("open").set_modified(long_ago).expect("age");
    drop(new_run(&root, "n1", &argv));
    assert!(!old.exists() && root.join("n1").exists());
}

// Checks where the store is: ZAZZLE_RUNS_DIR when set and not empty, else under HOME.
#[test]
fn test_runs_dir() {
    let _g = crate::testutil::env_lock();
    std::env::set_var("ZAZZLE_RUNS_DIR", "/tmp/somewhere");
    assert_eq!(dir(), Some(PathBuf::from("/tmp/somewhere")));
    std::env::set_var("ZAZZLE_RUNS_DIR", "");
    let home = dir();
    std::env::remove_var("ZAZZLE_RUNS_DIR");
    assert!(home.is_some_and(|h| h.ends_with(".zazzle/runs")));
}

// Checks cancelling: a run working here is told to stop, ends with what it had, and is kept as cancelled; a run that already ended is returned unchanged with a note; an unknown or bad id is refused.
#[test]
fn test_runs_cancel() {
    let t = TempDir::new();
    let root = t.path().join("runs");
    let argv = line(&["q"]);
    let job = new_run(&root, "c1", &argv);
    let worker = std::thread::spawn(move || {
        let stop = job.stop();
        crate::budget::stopping(&stop, || {
            while !crate::budget::cancelled() {
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        job.finish(&Ok(Output::plain("what it had".into()))).expect("finish");
    });
    assert_eq!(cancel(&root, "c1").expect("cancelled").text, "what it had");
    worker.join().expect("worker");
    let kept: Value = serde_json::from_str(&fs::read_to_string(root.join("c1/result.json")).expect("result"))
        .expect("json");
    assert_eq!(kept["state"], "cancelled");
    let again = cancel(&root, "c1").expect("ended");
    assert!(
        again.text == "what it had" && again.notes.contains("had already ended; nothing was cancelled"),
        "{again:?}"
    );
    assert_eq!(cancel(&root, "none"), Err("no investigate run none".into()));
    assert!(cancel(&root, "../x").unwrap_err().contains("--run-id takes"));
}

// Runs git in `dir` with a throwaway identity, for the checkout tests.
fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .expect("git")
        .status
        .success();
    assert!(ok, "git {args:?}");
}

// Checks a run id names one request in one checkout at one state: the same command line from another checkout, after an uncommitted change or after a new commit is refused, the id alone still looks it up, and a directory outside git reads as such.
#[test]
fn test_runs_identity() {
    let t = TempDir::new();
    let (root, repo, other) = (t.path().join("runs"), t.path().join("repo"), t.path().join("other"));
    fs::create_dir_all(&repo).expect("repo");
    fs::create_dir_all(&other).expect("other");
    git(&repo, &["init", "-q"]);
    assert_eq!(snapshot(&repo), "no git checkout", "no commit yet");
    fs::write(repo.join("a.cs"), "class A {}\n").expect("file");
    git(&repo, &["add", "a.cs"]);
    git(&repo, &["commit", "-q", "-m", "a"]);
    let head = snapshot(&repo);
    assert_eq!(head.len(), 40, "{head}");
    let argv = line(&["q"]);
    let until = || Instant::now() + Duration::from_secs(30);
    match begin(&root, "c1", &argv, &repo, until()).expect("begin") {
        Begun::New(j) => j.finish(&Ok(Output::plain("answer".into()))).expect("finish"),
        Begun::Earlier(_) => panic!("new"),
    }
    let again = |cwd: &Path, argv: &[String]| match begin(&root, "c1", argv, cwd, until()) {
        Ok(Begun::Earlier(r)) => r,
        Ok(Begun::New(_)) => panic!("started again"),
        Err(e) => Err(e),
    };
    assert_eq!(again(&repo, &argv).expect("same").text, "answer");
    let link = t.path().join("link");
    std::os::unix::fs::symlink(&repo, &link).expect("symlink");
    assert_eq!(again(&link, &argv).expect("through a symlink").text, "answer", "the same checkout");
    let moved = again(&other, &argv).unwrap_err();
    assert!(
        moved.contains("was started in another checkout (") && moved.contains("--run-id c1` alone"),
        "{moved}"
    );
    fs::write(repo.join("a.cs"), "class A { int x; }\n").expect("edit");
    let dirty = snapshot(&repo);
    assert!(dirty.starts_with(&format!("{head} with changes ")), "{dirty}");
    let changed = again(&repo, &argv).unwrap_err();
    assert!(
        changed.contains(&format!("on another state of this checkout ({head} then, {dirty} now)")),
        "{changed}"
    );
    assert_eq!(again(&other, &[]).expect("id alone").text, "answer");
    git(&repo, &["commit", "-q", "-am", "b"]);
    assert!(again(&repo, &argv).unwrap_err().contains("another state"), "a new commit");
    let request = root.join("c1").join("request.json");
    let mut old: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&request).expect("request")).expect("json");
    old.as_object_mut().expect("object").remove("snapshot");
    fs::write(&request, old.to_string()).expect("legacy request");
    assert_eq!(
        again(&repo, &argv).expect("legacy").text,
        "answer",
        "a run kept before snapshots is not refused for one"
    );
}

// Checks the evidence a run keeps as it works is listed when it stops without a result.
#[test]
fn test_runs_keep_evidence() {
    let t = TempDir::new();
    let root = t.path().join("runs");
    let job = new_run(&root, "e1", &line(&["q"]));
    append(&job.evidence(), &json!({"lookup": "READ a.cs:1-4", "excerpt": "   1\tclass A"})).expect("lookup");
    append(&job.evidence(), &json!({"finding": "A is a class", "citation": "a.cs:1", "by": "qwen low"}))
        .expect("finding");
    append(&job.evidence(), &json!({"other": 1})).expect("other");
    drop(job);
    let o = earlier(&root, "e1", &[]).expect("interrupted");
    assert!(
        o.text.contains("evidence kept before it stopped (3 records, in ")
            && o.text.contains(
                "- READ a.cs:1-4 found something\n- verified by qwen low: A is a class (a.cs:1)\nusage"
            ),
        "{}",
        o.text
    );
    let none = new_run(&root, "e2", &line(&["q"]));
    drop(none);
    assert!(!earlier(&root, "e2", &[]).expect("interrupted").text.contains("evidence kept"));
}
