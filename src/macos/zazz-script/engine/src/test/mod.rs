// zazz::test: a harness that runs a script's handlers the way the launcher does, over faked edges.
use crate::call::{Call, Clock, Env, Mode};
use crate::cliargs::{Parsed, Spec};
use crate::error::Result;
use crate::handlers::Handlers;
use crate::http::NetPolicy;
use crate::json::Json;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::rc::Rc;

mod check;
mod fakes;

pub use crate::assert_json_eq;
pub use check::{assert_snapshot, check_schema, json_diff, response_schema};
pub use fakes::{normalize, FakeFs, FakeHttp, FsPolicy};

// A writer whose bytes the harness can read back.
#[derive(Clone, Default)]
struct Capture(Rc<RefCell<Vec<u8>>>);

impl Write for Capture {
    // Appends bytes.
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }

    // Nothing to flush.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// A clock the test sets.
struct FakeClock(Rc<Cell<u64>>);

impl Clock for FakeClock {
    // The time the test set.
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
}

// Environment variables the test sets.
struct FakeEnv(Rc<RefCell<BTreeMap<String, String>>>);

impl Env for FakeEnv {
    // A variable the test set.
    fn var(&self, key: &str) -> Option<String> {
        self.0.borrow().get(key).cloned()
    }
}

// What the last call did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub exit: i64,
    pub progress: Vec<String>,
}

// Runs a script's handlers in-process with fake filesystem, network, stdio, clock and environment.
pub struct Harness {
    handlers: Handlers,
    fs: FakeFs,
    http: FakeHttp,
    clock: Rc<Cell<u64>>,
    env: Rc<RefCell<BTreeMap<String, String>>>,
    stdin: Vec<u8>,
    cwd: String,
    cancel_after: Option<usize>,
    cli: Option<Spec>,
    last: Outcome,
}

impl Harness {
    // A harness with everything allowed: fake fs and network are open, the clock is at 2024-01-01.
    pub fn new(handlers: Handlers) -> Harness {
        let fs = FakeFs::default();
        fs.set_cwd("/work");
        Harness {
            handlers,
            fs,
            http: FakeHttp::default(),
            clock: Rc::new(Cell::new(1_704_067_200_000)),
            env: Rc::default(),
            stdin: Vec::new(),
            cwd: "/work".into(),
            cancel_after: None,
            cli: None,
            last: Outcome::default(),
        }
    }

    // A harness that applies the manifest's sandbox and cli spec, as production would (pass include_str!("../zazz.json")).
    pub fn from_manifest(handlers: Handlers, manifest: &str) -> Harness {
        let m = Json::parse(manifest).unwrap_or_else(|e| panic!("zazz::test: zazz.json: {e}"));
        let mut h = Harness::new(handlers);
        let sb = &m["sandbox"];
        let list = |key: &str, default: &[&str]| {
            sb[key].arr().map_or_else(
                || default.iter().map(|s| s.to_string()).collect(),
                |a| a.iter().filter_map(|v| v.str().map(str::to_string)).collect(),
            )
        };
        h.fs.set_policy(FsPolicy { read: list("read", &["$CWD"]), write: list("write", &[]) });
        h.http.set_policy(match &sb["network"] {
            Json::Str(s) if s == "all" => NetPolicy::All,
            Json::Obj(_) => NetPolicy::Hosts(
                sb["network"]["hosts"]
                    .arr()
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|v| v.str().map(str::to_string))
                    .collect(),
            ),
            _ => NetPolicy::None,
        });
        if let Some(cli) = m.get("cli") {
            let spec = Spec::from_json(m.str_or("name", "script"), cli)
                .unwrap_or_else(|e| panic!("zazz::test: {e}"));
            h.cli = Some(spec);
        }
        h
    }

    // The fake filesystem.
    pub fn fs(&self) -> &FakeFs {
        &self.fs
    }

    // The fake network.
    pub fn http(&self) -> &FakeHttp {
        &self.http
    }

    // Sets standard input for the next calls.
    pub fn stdin(&mut self, data: impl AsRef<[u8]>) -> &mut Harness {
        self.stdin = data.as_ref().to_vec();
        self
    }

    // Sets the clock.
    pub fn clock_ms(&mut self, ms: u64) -> &mut Harness {
        self.clock.set(ms);
        self
    }

    // Moves the clock forward.
    pub fn advance_ms(&mut self, ms: u64) -> &mut Harness {
        self.clock.set(self.clock.get() + ms);
        self
    }

    // Sets an environment variable.
    pub fn env(&mut self, key: &str, value: &str) -> &mut Harness {
        self.env.borrow_mut().insert(key.into(), value.into());
        self
    }

    // Makes call.cancelled() return true after it has been asked n times.
    pub fn cancel_after(&mut self, n: usize) -> &mut Harness {
        self.cancel_after = Some(n);
        self
    }

    // The directory paths are resolved against for human runs (default /work).
    pub fn cwd(&mut self, dir: &str) -> &mut Harness {
        self.cwd = dir.into();
        self.fs.set_cwd(dir);
        self
    }

    // What the last call printed, its exit status and progress notes.
    pub fn last(&self) -> &Outcome {
        &self.last
    }

    // Builds a call over the fakes.
    fn call(&self, mode: Mode, out: &Capture, err: &Capture, progress: &Rc<RefCell<Vec<String>>>) -> Call {
        let notes = progress.clone();
        let probes = Rc::new(Cell::new(0usize));
        let limit = self.cancel_after;
        Call {
            id: 1,
            mode,
            exit: 0,
            stdin: Box::new(std::io::Cursor::new(self.stdin.clone())),
            stdout: Box::new(out.clone()),
            stderr: Box::new(err.clone()),
            fs: Box::new(self.fs.clone()),
            http: Box::new(self.http.clone()),
            clock: Box::new(FakeClock(self.clock.clone())),
            env: Box::new(FakeEnv(self.env.clone())),
            progress_sink: Box::new(move |m| notes.borrow_mut().push(m.to_string())),
            cancel_probe: Box::new(move || {
                probes.set(probes.get() + 1);
                limit.is_some_and(|n| probes.get() > n)
            }),
        }
    }

    // Runs one call through a closure and records what it printed.
    fn run(&mut self, mode: Mode, f: impl FnOnce(&Handlers, &mut Call) -> Result<Json>) -> Result<Json> {
        let (out, err, notes) = (Capture::default(), Capture::default(), Rc::new(RefCell::new(Vec::new())));
        let mut call = self.call(mode, &out, &err, &notes);
        let r = f(&self.handlers, &mut call);
        let exit = call.exit;
        drop(call);
        let text = |c: &Capture| String::from_utf8_lossy(&c.0.borrow()).into_owned();
        self.last =
            Outcome { stdout: text(&out), stderr: text(&err), exit, progress: notes.borrow().clone() };
        r
    }

    // Runs json-command as an agent would.
    pub fn json_command(&mut self, input: Json) -> Result<Json> {
        self.run(Mode::Agent, |h, c| h.command(c, input))
    }

    // Runs a service-call operation.
    pub fn service_call(&mut self, op: &str, params: Json, body: Json) -> Result<Json> {
        self.run(Mode::Agent, |h, c| h.service_call(c, op, &params, body))
    }

    // Returns the script's OpenAPI document.
    pub fn openapi(&self) -> Result<Json> {
        self.handlers.openapi_doc()
    }

    // Runs the script as a human would: parses argv with the manifest's cli spec, then renders the output.
    pub fn human(&mut self, argv: &[&str]) -> Outcome {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let parsed = match &self.cli {
            Some(spec) => spec.parse(&argv, &self.cwd),
            None => Ok(Parsed::Input(crate::json!({"argv": (argv)}))),
        };
        let input = match parsed {
            Ok(Parsed::Input(j)) => j,
            Ok(Parsed::Help(text)) => return self.set_last(text + "\n", String::new(), 0),
            Err(e) => return self.set_last(String::new(), e + "\n", 2),
        };
        let r = self.run(Mode::Human, |h, c| h.command(c, input));
        match r {
            Ok(out) => {
                let rendered = crate::render::render(&out, false);
                self.last.stdout.push_str(&rendered);
            }
            Err(e) => {
                self.last.stderr.push_str(&format!("{}: {}\n", e.code, e.message));
                self.last.exit = self.last.exit.max(1);
            }
        }
        self.last.clone()
    }

    // Records an outcome that never reached the script.
    fn set_last(&mut self, stdout: String, stderr: String, exit: i64) -> Outcome {
        self.last = Outcome { stdout, stderr, exit, progress: Vec::new() };
        self.last.clone()
    }

    // Asserts an output matches the response schema of an operation in the script's OpenAPI document.
    pub fn assert_schema(&self, output: &Json, operation: &str) {
        let doc = self.openapi().unwrap_or_else(|e| panic!("zazz::test: no openapi document: {e}"));
        let schema = response_schema(&doc, operation)
            .unwrap_or_else(|| panic!("zazz::test: no JSON response schema for '{operation}'"));
        let errs = check_schema(output, schema, &doc);
        assert!(errs.is_empty(), "output does not match the '{operation}' schema:\n{}", errs.join("\n"));
    }
}

#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// Serialises tests that change process environment variables.
#[cfg(test)]
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::call::Http;
    use crate::error::Error;
    use crate::http::Request;
    use crate::json;

    // A small script: sums numbers in files, fetches a URL, echoes env and clock, and handles cancellation.
    fn handlers() -> Handlers {
        Handlers::new()
            .json_command(|call, input| match input.str_or("do", "sum") {
                "fetch" => Ok(call.http().send(&Request::get(input.need_str("url")?))?.body_json()?),
                "env" => Ok(json!({"user": (call.env().var("USER")), "now": (call.clock().now_ms())})),
                "loop" => {
                    for i in 0..10i64 {
                        if call.cancelled() {
                            return Ok(Json::Int(i));
                        }
                    }
                    Ok(Json::Int(10))
                }
                "write" => {
                    call.fs().write(input.need_str("path")?, b"ok")?;
                    call.progress("wrote");
                    Ok(Json::Null)
                }
                "fail" => {
                    call.set_exit(3);
                    Err(Error::failed("broken"))
                }
                _ => {
                    let mut total = 0i64;
                    for f in input.strs("files")? {
                        let text = String::from_utf8(call.read_input(&f)?).map_err(|_| Error::bad_input("utf8"))?;
                        total += text.split_whitespace().filter_map(|n| n.parse::<i64>().ok()).sum::<i64>();
                    }
                    if call.is_human() {
                        call.println(&format!("total {total}"))?;
                        return Ok(Json::Null);
                    }
                    Ok(json!({"total": total}))
                }
            })
            .openapi(|| json!({"paths": {"/": {"post": {"operationId": "json-command", "responses": {"200": {"content": {"application/json": {"schema": {"type": "object", "required": ["total"], "properties": {"total": {"type": "integer"}}}}}}}}}}}))
            .service("double", |_, p, _| Ok(Json::Int(p.need_i64("n")? * 2)))
    }

    const MANIFEST: &str = r#"{"zazz":1,"name":"sum","kind":"rust","entry":"src/main.rs",
        "sandbox":{"read":["$CWD"],"write":["$CWD/out"],"network":{"hosts":["api.x.com"]}},
        "cli":{"args":[{"name":"files","positional":true,"multiple":true,"type":"path","required":true}]}}"#;

    // Checks agent calls over the fake filesystem and stdin, plus the schema assertion.
    #[test]
    fn test_harness_agent_calls() {
        let mut h = Harness::new(handlers());
        h.fs().add("a.txt", "1 2").add("b.txt", "3");
        let out = h.json_command(json!({"files": ["a.txt", "b.txt"]})).expect("sum");
        assert_json_eq!(out.clone(), json!({"total": 6}));
        h.assert_schema(&out, "json-command");
        h.stdin("10 20");
        assert_eq!(h.json_command(json!({"files": "-"})).expect("stdin")["total"], Json::Int(30));
        assert_eq!(h.json_command(json!({"files": "missing"})).unwrap_err().code, "not_found");
        assert_eq!(h.service_call("double", json!({"n": 4}), Json::Null).expect("svc"), Json::Int(8));
        assert!(h.openapi().is_ok());
    }

    // Checks the clock, environment, progress, writes and cancellation fakes.
    #[test]
    fn test_harness_fakes() {
        let mut h = Harness::new(handlers());
        h.env("USER", "kim").clock_ms(5).advance_ms(5);
        assert_json_eq!(
            h.json_command(json!({"do": "env"})).expect("env"),
            json!({"user": "kim", "now": 10})
        );
        h.json_command(json!({"do": "write", "path": "x"})).expect("write");
        assert_eq!(
            (h.fs().written_text("x").as_deref(), h.last().progress.clone()),
            (Some("ok"), vec!["wrote".to_string()])
        );
        assert_eq!(h.json_command(json!({"do": "loop"})).expect("loop"), Json::Int(10));
        h.cancel_after(3);
        assert_eq!(h.json_command(json!({"do": "loop"})).expect("loop"), Json::Int(3));
        h.http().expect_json("GET", "https://api.x.com/v", 200, json!({"v": 1}));
        assert_eq!(
            h.json_command(json!({"do": "fetch", "url": "https://api.x.com/v"})).expect("fetch")["v"],
            Json::Int(1)
        );
        h.http().assert_all_used();
    }

    // Checks the manifest's sandbox is applied to the fakes.
    #[test]
    fn test_harness_from_manifest_policy() {
        let mut h = Harness::from_manifest(handlers(), MANIFEST);
        assert_eq!(h.json_command(json!({"do": "write", "path": "x"})).unwrap_err().code, "sandbox_denied");
        assert!(h.json_command(json!({"do": "write", "path": "out/x"})).is_ok());
        assert_eq!(
            h.json_command(json!({"do": "fetch", "url": "https://evil.com/"})).unwrap_err().code,
            "network_denied"
        );
        let all = r#"{"name":"s","sandbox":{"network":"all"}}"#;
        let mut h = Harness::from_manifest(handlers(), all);
        h.http().expect_json("GET", "https://evil.com/", 200, json!(1));
        assert!(h.json_command(json!({"do": "fetch", "url": "https://evil.com/"})).is_ok());
        let none = Harness::from_manifest(handlers(), r#"{"name":"s"}"#);
        assert_eq!(none.http().send(&Request::get("https://a.com/")).unwrap_err().code, "network_denied");
    }

    // Checks human runs: parsing, help, parse errors, script errors and rendering.
    #[test]
    fn test_harness_human() {
        let mut h = Harness::from_manifest(handlers(), MANIFEST);
        h.fs().add("/work/a.txt", "4 5");
        let o = h.human(&["a.txt"]);
        assert_eq!((o.stdout.as_str(), o.exit), ("total 9\n", 0));
        assert!(h.human(&["--help"]).stdout.starts_with("usage: sum"));
        let e = h.human(&[]);
        assert_eq!((e.exit, e.stderr.contains("is required")), (2, true));
        let f = h.human(&["missing.txt"]);
        assert_eq!((f.exit, f.stderr.starts_with("not_found: ")), (1, true));
        let mut plain = Harness::new(handlers());
        plain.cwd("/w");
        assert_eq!(plain.human(&["x"]).exit, 1);
        let mut failing = Harness::new(Handlers::new().json_command(|c, _| {
            c.set_exit(3);
            Err(Error::failed("broken"))
        }));
        assert_eq!(failing.human(&[]).exit, 3);
        let mut rendering = Harness::new(Handlers::new().json_command(|_, input| Ok(input)));
        assert_eq!(rendering.human(&["a"]).stdout, "{\n  \"argv\": [\n    \"a\"\n  ]\n}\n");
    }

    // Checks schema mismatches and missing documents panic with explanations.
    #[test]
    fn test_harness_schema_failures() {
        let h = Harness::new(handlers());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || h.assert_schema(&json!({}), "json-command")
        ))
        .is_err());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || h.assert_schema(&json!({}), "nope")
        ))
        .is_err());
        let bare = Harness::new(Handlers::new());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || bare.assert_schema(&json!({}), "x")
        ))
        .is_err());
    }

    // Checks bad manifests and cli specs fail loudly.
    #[test]
    fn test_harness_bad_manifest() {
        assert!(std::panic::catch_unwind(|| Harness::from_manifest(Handlers::new(), "{")).is_err());
        let bad_cli = r#"{"name":"s","cli":{"stdin":"maybe"}}"#;
        assert!(std::panic::catch_unwind(|| Harness::from_manifest(Handlers::new(), bad_cli)).is_err());
        let mut c = Capture::default();
        c.flush().expect("flush");
    }
}
