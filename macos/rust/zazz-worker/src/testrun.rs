// Test mode: runs a script's test module, one fresh instance per test, and reports passes and failures as JSON.
use crate::net::Replay;
use crate::run::{self, Ending, Hooks, Io, Preopen, Setup};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use wasmtime::Module;
use wasmtime_wasi::p2::pipe::{ClosedInputStream, MemoryOutputPipe};
use zazz::http::NetPolicy;
use zazz::{json, proto, Json};

const MAX_FAILURES: usize = 20;
const MAX_MESSAGE: usize = 1500;
const CAPTURE: usize = 1 << 20;

// How to run a test module.
#[derive(Debug)]
pub struct Opts {
    pub libtest: bool,
    pub cwd: PathBuf,
    pub snapshots: Option<PathBuf>,
    pub update: bool,
    pub replay: Replay,
    pub timeout_ms: u64,
    pub memory_mb: u64,
}

// One run's captured output and ending.
struct Run {
    stdout: String,
    stderr: String,
    sent: Vec<Json>,
    ending: Ending,
}

// Runs the module once with arguments, capturing output and using the replay file as the network.
fn run_once(
    module: &Module,
    opts: &Opts,
    args: &[&str],
    ms: u64,
    replay: &Arc<Replay>,
) -> Result<Run, String> {
    let mut preopens = vec![Preopen { host: opts.cwd.clone(), guest: ".".into(), write: true }];
    let mut env = vec![("RUST_BACKTRACE".to_string(), "0".to_string())];
    if let Some(s) = &opts.snapshots {
        preopens.push(Preopen { host: s.clone(), guest: "/snapshots".into(), write: opts.update });
        env.push(("ZAZZ_SNAPSHOTS".into(), "/snapshots".into()));
    }
    if opts.update {
        env.push(("ZAZZ_UPDATE_SNAPSHOTS".into(), "1".into()));
    }
    let setup = Setup {
        args: std::iter::once("tests").chain(args.iter().copied()).map(str::to_string).collect(),
        env,
        preopens,
        net: NetPolicy::All,
        memory_mb: opts.memory_mb,
        call_ms: ms.max(1),
    };
    let (out, err) = (MemoryOutputPipe::new(CAPTURE), MemoryOutputPipe::new(CAPTURE));
    let io = Io {
        stdin: Box::new(ClosedInputStream),
        stdout: Box::new(out.clone()),
        stderr: Box::new(err.clone()),
    };
    let hooks = Hooks { cancelled: Box::new(|| false), progress: Box::new(|_| {}), backend: replay.clone() };
    let o = run::run(&run::linker(), module, &setup, b"{}", io, hooks)?;
    let text = |p: &MemoryOutputPipe| String::from_utf8_lossy(&p.contents()).into_owned();
    Ok(Run { stdout: text(&out), stderr: text(&err), sent: o.sent, ending: o.ending })
}

// Lists test names from libtest's terse listing.
pub fn parse_list(stdout: &str) -> Vec<String> {
    stdout.lines().filter_map(|l| l.strip_suffix(": test")).map(str::to_string).collect()
}

// Pulls the panic message, file and line out of a failed test's output.
pub fn failure(name: &str, run_stdout: &str, run_stderr: &str, ending: &Ending) -> Json {
    let all = format!("{run_stderr}\n{run_stdout}");
    let at = all.lines().position(|l| l.contains("panicked at "));
    let (file, line, message) = match at {
        Some(i) => {
            let lines: Vec<&str> = all.lines().collect();
            let loc = lines[i].split("panicked at ").nth(1).unwrap_or("").trim_end_matches(':');
            let mut parts = loc.split(':');
            let file = parts.next().unwrap_or("").replace("src/../", "");
            let line = parts.next().and_then(|n| n.parse::<i64>().ok());
            let msg: Vec<&str> = lines[i + 1..]
                .iter()
                .take_while(|l| !l.starts_with("note:") && !l.is_empty())
                .copied()
                .collect();
            (Some(file), line, msg.join("\n"))
        }
        None => (
            None,
            None,
            match ending {
                Ending::Timeout => "the test ran past the time limit".into(),
                Ending::OutOfMemory => "the test ran past the memory limit".into(),
                _ => all
                    .trim()
                    .lines()
                    .rev()
                    .take(10)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n"),
            },
        ),
    };
    let message: String = message.chars().take(MAX_MESSAGE).collect();
    json!({"name": name, "message": message, "file": file, "line": line})
}

// Runs every libtest test in its own instance (WASM panics abort, so one instance cannot survive a failure).
fn run_libtest(module: &Module, opts: &Opts, replay: &Arc<Replay>) -> Result<Json, String> {
    let started = Instant::now();
    let budget = |o: &Opts| o.timeout_ms.saturating_sub(started.elapsed().as_millis() as u64);
    let listed = run_once(module, opts, &["--list", "--format", "terse"], budget(opts), replay)?;
    if listed.ending != Ending::Exit(0) {
        return Err(format!("listing tests failed: {}", listed.stderr.trim()));
    }
    let (mut passed, mut ignored, mut failed) = (0, 0, Vec::new());
    for name in parse_list(&listed.stdout) {
        let r = run_once(module, opts, &[&name, "--exact", "--nocapture"], budget(opts), replay)?;
        if r.ending == Ending::Exit(0) && r.stdout.contains(&format!("test {name} ... ignored")) {
            ignored += 1;
        } else if r.ending == Ending::Exit(0) {
            passed += 1;
        } else {
            failed.push(failure(&name, &r.stdout, &r.stderr, &r.ending));
        }
    }
    failed.truncate(MAX_FAILURES);
    Ok(
        json!({"passed": passed, "ignored": ignored, "failed": failed, "duration_ms": (started.elapsed().as_millis() as u64)}),
    )
}

// Runs a test command once; a test-report message wins, otherwise the exit status decides.
fn run_command(module: &Module, opts: &Opts, replay: &Arc<Replay>) -> Result<Json, String> {
    let started = Instant::now();
    let r = run_once(module, opts, &[], opts.timeout_ms, replay)?;
    if let Some(rep) = r.sent.iter().rev().find(|m| proto::kind(m) == "test-report") {
        let failed = rep["failed"].arr().map(<[Json]>::to_vec).unwrap_or_default();
        return Ok(
            json!({"passed": (rep["passed"].i64().unwrap_or(0)), "ignored": (rep["ignored"].i64().unwrap_or(0)), "failed": failed, "duration_ms": (started.elapsed().as_millis() as u64)}),
        );
    }
    let failed = if r.ending == Ending::Exit(0) {
        vec![]
    } else {
        vec![failure("tests", &r.stdout, &r.stderr, &r.ending)]
    };
    let passed = failed.is_empty() as i64;
    Ok(
        json!({"passed": passed, "ignored": 0, "failed": failed, "duration_ms": (started.elapsed().as_millis() as u64)}),
    )
}

// Runs a test module and returns the report.
pub fn run_tests(module: &Module, opts: Opts) -> Result<Json, String> {
    let replay = Arc::new(Replay(opts.replay.0.clone()));
    if opts.libtest {
        run_libtest(module, &opts, &replay)
    } else {
        run_command(module, &opts, &replay)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks the libtest listing parser.
    #[test]
    fn test_parse_list() {
        assert_eq!(parse_list("a::b: test\nc: bench\nd: test\n\n2 tests, 0 benchmarks\n"), vec!["a::b", "d"]);
    }

    // Checks panic messages are extracted with file and line.
    #[test]
    fn test_failure_extraction() {
        let err = "thread 'main' panicked at src/../tests/basic.rs:12:5:\nassertion `left == right` failed: deliberate\n  left: 1\n right: 2\nnote: run with RUST_BACKTRACE=1\n";
        let f = failure("t::x", "", err, &Ending::Crash("unreachable".into()));
        assert_eq!((f["file"].str(), f["line"].i64()), (Some("tests/basic.rs"), Some(12)));
        assert_eq!(
            f["message"].str(),
            Some("assertion `left == right` failed: deliberate\n  left: 1\n right: 2")
        );
        assert_eq!(
            failure("t", "", "", &Ending::Timeout)["message"].str(),
            Some("the test ran past the time limit")
        );
        assert_eq!(
            failure("t", "", "", &Ending::OutOfMemory)["message"].str(),
            Some("the test ran past the memory limit")
        );
        assert_eq!(failure("t", "a\nb", "", &Ending::Exit(1))["message"].str(), Some("a\nb"));
        assert_eq!(failure("t", "", "", &Ending::Exit(1))["file"], Json::Null);
    }

    // Loads a WAT guest.
    fn module(text: &str) -> Module {
        crate::engine::load(&crate::engine::compile(&wat::parse_str(text).expect("wat")).expect("compile"))
            .expect("load")
    }

    // Default options over a temporary directory.
    fn opts(libtest: bool) -> Opts {
        Opts {
            libtest,
            cwd: std::env::temp_dir(),
            snapshots: Some(std::env::temp_dir()),
            update: true,
            replay: Replay(vec![]),
            timeout_ms: 2000,
            memory_mb: 16,
        }
    }

    // Checks command-mode tests: exit status decides, and a test-report message wins.
    #[test]
    fn test_command_mode() {
        let ok = r#"(module (memory (export "memory") 1) (func (export "_start")))"#;
        let r = run_tests(&module(ok), opts(false)).expect("ok");
        assert_eq!((r["passed"].i64(), r["failed"].arr().map(<[Json]>::len)), (Some(1), Some(0)));
        let bad = r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#;
        let r = run_tests(&module(bad), opts(false)).expect("bad");
        assert_eq!((r["passed"].i64(), r["failed"][0]["name"].str()), (Some(0), Some("tests")));
        let report = r#"(module (import "zazz_host" "send" (func $send (param i32 i32))) (memory (export "memory") 1)
          (data (i32.const 0) "{\"t\":\"test-report\",\"passed\":4,\"failed\":[{\"name\":\"x\"}]}")
          (func (export "_start") (call $send (i32.const 0) (i32.const 54))))"#;
        let r = run_tests(&module(report), opts(false)).expect("report");
        assert_eq!(
            (r["passed"].i64(), r["failed"][0]["name"].str(), r["ignored"].i64()),
            (Some(4), Some("x"), Some(0))
        );
    }

    // A fake libtest binary: lists a, b and c; a passes, b panics, c is ignored.
    const FAKE_LIBTEST: &str = r#"(module
      (import "wasi_snapshot_preview1" "args_sizes_get" (func $sizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "args_get" (func $get (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_write" (func $w (param i32 i32 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
      (memory (export "memory") 1)
      (data (i32.const 1000) "a: test\0ab: test\0ac: test\0a")
      (data (i32.const 1100) "test a ... ok\0a")
      (data (i32.const 1200) "test c ... ignored\0a")
      (data (i32.const 1300) "thread 'main' panicked at src/../tests/t.rs:3:5:\0aboom\0a")
      (func $print (param $fd i32) (param $ptr i32) (param $len i32)
        (i32.store (i32.const 0) (local.get $ptr)) (i32.store (i32.const 4) (local.get $len))
        (drop (call $w (local.get $fd) (i32.const 0) (i32.const 1) (i32.const 8))))
      (func (export "_start")
        (local $c i32)
        (drop (call $sizes (i32.const 16) (i32.const 20)))
        (drop (call $get (i32.const 100) (i32.const 200)))
        (local.set $c (i32.load8_u (i32.load (i32.const 104))))
        (if (i32.eq (local.get $c) (i32.const 45)) (then (call $print (i32.const 1) (i32.const 1000) (i32.const 24)) (return)))
        (if (i32.eq (local.get $c) (i32.const 97)) (then (call $print (i32.const 1) (i32.const 1100) (i32.const 14)) (return)))
        (if (i32.eq (local.get $c) (i32.const 99)) (then (call $print (i32.const 1) (i32.const 1200) (i32.const 19)) (return)))
        (call $print (i32.const 2) (i32.const 1300) (i32.const 54))
        (call $exit (i32.const 101))))"#;

    // Checks libtest mode runs each listed test in its own instance and classifies the results.
    #[test]
    fn test_libtest_mode_runs_each_test() {
        let mut o = opts(true);
        o.snapshots = None;
        o.update = false;
        let r = run_tests(&module(FAKE_LIBTEST), o).expect("run");
        assert_eq!((r["passed"].i64(), r["ignored"].i64()), (Some(1), Some(1)));
        assert_eq!(
            (r["failed"][0]["name"].str(), r["failed"][0]["message"].str()),
            (Some("b"), Some("boom"))
        );
        assert_eq!(r["failed"][0]["file"].str(), Some("tests/t.rs"));
    }

    // Checks libtest mode refuses a module whose listing fails, and handles an empty listing.
    #[test]
    fn test_libtest_mode_listing() {
        let bad = r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#;
        assert!(run_tests(&module(bad), opts(true)).unwrap_err().starts_with("listing tests failed"));
        let empty = r#"(module (memory (export "memory") 1) (func (export "_start")))"#;
        let r = run_tests(&module(empty), opts(true)).expect("empty");
        assert_eq!(r["passed"].i64(), Some(0));
    }
}
