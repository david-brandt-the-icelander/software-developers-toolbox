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
    fs::write(gone.join(REQUEST), json!({"argv": ["q"]}).to_string()).expect("request");
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
