// The zazz subcommands: pack, add, test and call sandboxed scripts.
use crate::cmd::{Ctx, Output};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

pub mod add;
pub mod call;
pub mod compile;
pub mod manage;
pub mod pack;
pub mod pipeline;
pub mod pool;
pub mod proc;
pub mod registry;
pub mod sdk;
pub mod testcmd;
pub mod tools;

const HELP: &str = "zazz: sandboxed scripts for agents (JSON replies)
pack DIR [-o FILE]       pack a script directory into a deterministic .zazz (default DIR.zazz beside DIR)
test DIR|FILE [--update]  build and run the script's tests; nothing is registered (--update rewrites snapshots)
add FILE.zazz            build, test once and register; replies with the script id (b3:...)
call ID [JSON|-]         run the script's json-command with JSON input (- reads stdin); replies with its output
call ID OP [--params JSON] [--body JSON|-]  run one service-call operation
ls                       registered scripts
describe ID              what a script is, how it runs and what it may touch
rm ID                    remove a registered script";
const COMMANDS: &str = "pack test add call ls describe rm help";

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

// The zazz state kept by the daemon: where the registry lives and which worker binary to use.
#[derive(Default)]
pub struct Zazz {
    root: Mutex<Option<PathBuf>>,
    worker: Mutex<Option<PathBuf>>,
    pub pool: std::sync::Arc<pool::Pool>,
}

impl Zazz {
    // Uses a fixed registry directory instead of the default.
    pub fn set_root(&self, p: &Path) {
        *self.root.lock().expect("zazz root lock") = Some(p.to_path_buf());
    }

    // Uses a fixed zazz-worker binary instead of looking for one.
    pub fn set_worker(&self, p: &Path) {
        *self.worker.lock().expect("zazz worker lock") = Some(p.to_path_buf());
    }

    // Opens the registry.
    pub fn registry(&self) -> Result<registry::Registry, String> {
        let root = match self.root.lock().expect("zazz root lock").clone() {
            Some(r) => r,
            None => registry::default_root()?,
        };
        registry::Registry::open(&root)
    }

    // Finds zazz-worker, or explains what is missing.
    pub fn worker(&self) -> Result<PathBuf, Value> {
        let fixed = self.worker.lock().expect("zazz worker lock").clone();
        fixed.or_else(tools::worker).ok_or_else(|| {
            pipeline::err("no_worker", "zazz-worker was not found beside zazzled (or set ZAZZ_WORKER)")
        })
    }
}

// Turns a JSON result into an output: the value on success, {"error": ...} with status 2 on failure.
fn reply(r: Result<Value, Value>) -> Output {
    match r {
        Ok(v) => Output::plain(v.to_string()),
        Err(e) => Output { text: serde_json::json!({"error": e}).to_string(), status: 2, idable: false },
    }
}

// Routes a zazz subcommand.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<Output, String> {
    let Some((sub, rest)) = args.split_first() else { return Ok(Output::plain(HELP.into())) };
    let z = &ctx.engine.zazz;
    match sub.as_str() {
        "pack" => pack::run(ctx, rest),
        "add" => Ok(reply(add::run(z, &ctx.cwd, rest))),
        "call" => Ok(reply(call::run(ctx, rest))),
        "test" => Ok(reply(testcmd::run(z, &ctx.cwd, rest))),
        "ls" => Ok(reply(manage::ls(z))),
        "rm" => Ok(reply(manage::rm(z, rest))),
        "describe" => Ok(reply(manage::describe(z, rest))),
        "help" | "--help" | "-h" => Ok(Output::plain(HELP.into())),
        other => Err(format!("unknown zazz command '{other}'; try: {COMMANDS}")),
    }
}

// Returns the canonical directories zazz may write output files into: the caller's directory and temp dirs.
fn writable_roots(cwd: &Path) -> Vec<PathBuf> {
    [cwd.to_path_buf(), std::env::temp_dir(), PathBuf::from("/tmp")]
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect()
}

// Resolves an output path whose parent exists and lies under the caller's directory or a temp dir.
pub fn output_path(cwd: &Path, arg: &Path) -> Result<PathBuf, String> {
    let wanted = cwd.join(arg);
    let name = wanted.file_name().ok_or_else(|| format!("{}: not a file name", arg.display()))?;
    let parent = wanted.parent().unwrap_or(cwd);
    let real = parent.canonicalize().map_err(|e| format!("{}: {e}", parent.display()))?;
    if !writable_roots(cwd).iter().any(|r| real.starts_with(r)) {
        return Err(format!(
            "refusing to write {} outside the working directory or a temp directory",
            arg.display()
        ));
    }
    Ok(real.join(name))
}

// Writes a file atomically through a temporary name in the same directory.
pub fn write_new(path: &Path, data: &[u8]) -> Result<(), String> {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = path.with_file_name(format!(".zazz-tmp-{}-{n}", std::process::id()));
    let result = std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path));
    result.inspect_err(|_| drop(std::fs::remove_file(&tmp))).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{run_cmd, run_cmd_with, zazz_engine, TempDir};

    const MANIFEST: &str =
        r#"{"zazz":1,"name":"sum","kind":"rust","entry":"src/main.rs","tests":{"paths":["tests"]}}"#;
    const MAIN: &str = "// Sums numbers in files.
use zazz::{json, Call, Handlers, Json, Result};

// Adds up the integers in every file.
pub fn run(call: &mut Call, input: Json) -> Result<Json> {
    let mut total = 0i64;
    for f in input.strs(\"files\")? {
        let text = String::from_utf8_lossy(&call.read_input(&f)?).into_owned();
        total += text.split_whitespace().filter_map(|n| n.parse::<i64>().ok()).sum::<i64>();
    }
    Ok(json!({\"total\": total}))
}

// The handlers.
pub fn handlers() -> Handlers {
    Handlers::new().json_command(run)
}

// Entry point.
fn main() {
    zazz::serve(handlers());
}
";
    const TEST: &str = "use crate::script::handlers;
use zazz::json;
use zazz::test::*;

#[test]
fn sums() {
    let mut h = Harness::new(handlers());
    h.fs().add(\"a.txt\", \"1 2 3\");
    assert_json_eq!(h.json_command(json!({\"files\": [\"a.txt\"]})).unwrap(), json!({\"total\": 6}));
}

#[test]
fn snapshot() {
    let mut h = Harness::new(handlers());
    h.fs().add(\"b.txt\", \"4\");
    assert_snapshot(\"b\", &h.json_command(json!({\"files\": [\"b.txt\"]})).unwrap());
}
";

    // Writes the demo script directory.
    fn demo(t: &TempDir, test: &str) {
        t.write("sum/zazz.json", MANIFEST.as_bytes());
        t.write("sum/src/main.rs", MAIN.as_bytes());
        t.write("sum/tests/basic.rs", test.as_bytes());
    }

    // Parses a JSON reply.
    fn reply_of(o: &crate::cmd::Output) -> Value {
        serde_json::from_str(&o.text).unwrap_or_else(|_| panic!("not JSON: {}", o.text))
    }

    // Checks the agent loop end to end: test (with snapshot update), pack, add, cached add, ls, describe, rm.
    #[test]
    fn test_rust_script_end_to_end() {
        let t = TempDir::new();
        let e = zazz_engine(&t);
        demo(&t, TEST);
        let run = |args: &[&str]| run_cmd_with(&e, t.path(), "", args);
        let missing = reply_of(&run(&["zazz", "test", "sum"]));
        assert_eq!(missing["error"]["code"], "tests_failed", "{missing}");
        let tested = reply_of(&run(&["zazz", "test", "sum", "--update"]));
        assert_eq!(
            (tested["build"].as_str(), tested["tests"]["passed"].as_i64()),
            (Some("ok"), Some(2)),
            "{tested}"
        );
        assert!(t.path().join("sum/tests/snapshots/b.json").is_file());
        assert_eq!(reply_of(&run(&["zazz", "test", "sum"]))["tests"]["passed"], 2);
        let packed = reply_of(&run(&["zazz", "pack", "sum"]));
        let added = reply_of(&run(&["zazz", "add", "sum.zazz"]));
        assert_eq!(
            (added["status"].as_str(), added["hash"].clone()),
            (Some("compiled"), packed["hash"].clone()),
            "{added}"
        );
        assert_eq!(reply_of(&run(&["zazz", "add", "sum.zazz"]))["status"], "cached");
        assert_eq!(reply_of(&run(&["zazz", "test", "sum.zazz"]))["tests"]["passed"], 2);
        let ls = reply_of(&run(&["zazz", "ls"]));
        assert_eq!(
            (ls[0]["name"].as_str(), ls[0]["tests"].as_str()),
            (Some("sum"), Some("2 passed, 0 ignored"))
        );
        let id = added["hash"].as_str().expect("hash").to_string();
        t.write("a.txt", b"5 6");
        let called = reply_of(&run(&["zazz", "call", &id[..15], r#"{"files":["a.txt"]}"#]));
        assert_eq!(called, serde_json::json!({"total": 11}));
        assert_eq!(e.zazz.pool.idle_count(), 1);
        assert_eq!(reply_of(&run(&["zazz", "call", &id, r#"{"files":["a.txt"]}"#]))["total"], 11);
        assert_eq!(e.zazz.pool.idle_count(), 1);
        let escape = reply_of(&run(&["zazz", "call", &id, r#"{"files":["../../../../etc/hosts"]}"#]));
        assert_eq!(escape["error"]["code"], "sandbox_denied", "{escape}");
        let fed = crate::testutil::run_cmd_stdin(
            &e,
            t.path(),
            &["zazz", "call", &id, r#"{"files":["-"]}"#],
            b"1 1",
        );
        assert_eq!(reply_of(&fed)["total"], 2);
        assert!(call_with_pipes(&e, &id, t.path()).contains("\"total\":3"));
        assert_eq!(reply_of(&run(&["zazz", "call", &id, "nosuchop"]))["error"]["code"], "not_found");
        assert_eq!(e.zazz.pool.shutdown(), 1);
        let d = reply_of(&run(&["zazz", "describe", &id[..15]]));
        assert_eq!((d["name"].as_str(), d["sandbox"]["read"][0].as_str()), (Some("sum"), Some("$CWD")));
        assert_eq!(reply_of(&run(&["zazz", "rm", &id]))["removed"].as_str(), Some(id.as_str()));
        assert_eq!(reply_of(&run(&["zazz", "describe", &id]))["error"]["code"], "not_found");
    }

    // Runs a call with real caller streams (pipes) and returns what the script wrote to stdout plus the reply.
    fn call_with_pipes(e: &crate::engine::Engine, id: &str, cwd: &Path) -> String {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let (mut in_w, in_r) = UnixStream::pair().expect("in");
        let (mut out_r, out_w) = UnixStream::pair().expect("out");
        in_w.write_all(b"1 2").expect("stdin");
        drop(in_w);
        let err_w = out_w.try_clone().expect("err");
        let streams = call::Streams::Caller { stdio: [in_r.into(), out_w.into(), err_w.into()], conn: None };
        let msg = serde_json::json!({"t": "json-command", "mode": "agent", "input": {"files": ["-"]}});
        let hex = &id[3..];
        let called = call::execute(&e.zazz, hex, cwd, msg, streams).expect("call");
        let mut printed = String::new();
        out_r.read_to_string(&mut printed).expect("read");
        format!("{printed}{}", called.result)
    }

    // Checks compile errors come back as short diagnostics and failing tests as a report.
    #[test]
    fn test_rust_script_failures() {
        let t = TempDir::new();
        let e = zazz_engine(&t);
        demo(&t, "use crate::script::handlers;\n#[test]\nfn broken() { let _ = handlers(); assert_eq!(1, 2, \"deliberate\"); }\n");
        let run = |args: &[&str]| reply_of(&run_cmd_with(&e, t.path(), "", args));
        let failed = run(&["zazz", "test", "sum"]);
        let f = &failed["error"]["tests"]["failed"][0];
        assert_eq!(
            (f["name"].as_str(), f["file"].as_str()),
            (Some("basic::broken"), Some("tests/basic.rs")),
            "{failed}"
        );
        t.write("sum/src/main.rs", MAIN.replace("total += text", "total += nope").as_bytes());
        let broken = run(&["zazz", "test", "sum"]);
        let d = &broken["error"]["diagnostics"][0];
        assert_eq!(
            (broken["error"]["code"].as_str(), d["file"].as_str()),
            (Some("build_failed"), Some("src/main.rs")),
            "{broken}"
        );
        assert!(d["msg"].as_str().expect("msg").contains("nope"));
    }

    // Checks argument errors, unreadable files and invalid archives.
    #[test]
    fn test_zazz_errors() {
        let t = TempDir::new();
        let e = zazz_engine(&t);
        let run = |args: &[&str]| reply_of(&run_cmd_with(&e, t.path(), "", args));
        assert_eq!(run(&["zazz", "add"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "add", "missing.zazz"])["error"]["code"], "unreadable");
        t.write("junk.zazz", b"junk");
        assert_eq!(run(&["zazz", "add", "junk.zazz"])["error"]["code"], "invalid_archive");
        assert_eq!(run(&["zazz", "test", "junk.zazz"])["error"]["code"], "invalid_archive");
        assert_eq!(run(&["zazz", "test", "junk.zazz", "--update"])["error"]["code"], "usage");
        t.write("bad/zazz.json", b"{}");
        assert_eq!(run(&["zazz", "test", "bad"])["error"]["code"], "invalid_manifest");
        let z = zazz_format::zip::write(&zazz_format::zip::Archive::new(vec![zazz_format::zip::Entry {
            name: "zazz.json".into(),
            data: b"{}".to_vec(),
        }]))
        .expect("zip");
        t.write("nomani.zazz", &z);
        assert_eq!(run(&["zazz", "add", "nomani.zazz"])["error"]["code"], "invalid_manifest");
        assert_eq!(run(&["zazz", "test", "nomani.zazz"])["error"]["code"], "invalid_manifest");
        assert_eq!(run(&["zazz", "rm"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "describe"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "rm", "ffffffffffffffff"])["error"]["code"], "not_found");
        assert_eq!(run(&["zazz", "ls"]), Value::Array(vec![]));
    }

    // Checks a prebuilt WASM script registers, and a script whose build fails is not registered.
    #[test]
    fn test_wasm_script_add() {
        let t = TempDir::new();
        let e = zazz_engine(&t);
        let wasm =
            wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).expect("wat");
        t.write("w/zazz.json", br#"{"zazz":1,"name":"w","kind":"wasm","entry":"w.wasm"}"#);
        t.write("w/w.wasm", &wasm);
        let run = |args: &[&str]| reply_of(&run_cmd_with(&e, t.path(), "", args));
        run(&["zazz", "pack", "w"]);
        let added = run(&["zazz", "add", "w.zazz"]);
        assert_eq!(
            (added["status"].as_str(), added["warnings"].as_array().map(Vec::len)),
            (Some("compiled"), Some(1)),
            "{added}"
        );
        t.write("x/zazz.json", br#"{"zazz":1,"name":"x","kind":"wasm","entry":"x.wasm"}"#);
        t.write("x/x.wasm", b"not wasm");
        run(&["zazz", "pack", "x"]);
        assert_eq!(run(&["zazz", "add", "x.zazz"])["error"]["code"], "invalid_wasm");
        assert_eq!(run(&["zazz", "ls"]).as_array().map(Vec::len), Some(1));
    }

    // Checks the registry and worker overrides, and the missing-worker error.
    #[test]
    fn test_zazz_state() {
        let t = TempDir::new();
        let z = Zazz::default();
        z.set_root(t.path());
        assert_eq!(z.registry().expect("reg").root, t.path());
        let _g = crate::testutil::env_lock();
        std::env::set_var("ZAZZ_WORKER", "/nonexistent/zazz-worker");
        assert_eq!(Zazz::default().worker().unwrap_err()["code"], "no_worker");
        std::env::remove_var("ZAZZ_WORKER");
        z.set_worker(Path::new("/w"));
        assert_eq!(z.worker().expect("fixed"), PathBuf::from("/w"));
    }

    // Checks help and unknown subcommands.
    #[test]
    fn test_run_help_and_unknown() {
        let t = TempDir::new();
        for args in [vec!["zazz"], vec!["zazz", "help"]] {
            assert!(run_cmd(t.path(), &args).text.starts_with("zazz: sandboxed scripts"));
        }
        let o = run_cmd(t.path(), &["zazz", "nope"]);
        assert_eq!((o.status, o.text.contains("unknown zazz command 'nope'")), (2, true));
    }

    // Checks output paths are confined to the working directory and temp directories.
    #[test]
    fn test_output_path_confinement() {
        let t = TempDir::new();
        assert_eq!(output_path(t.path(), Path::new("a.zazz")).expect("cwd"), t.path().join("a.zazz"));
        assert!(output_path(t.path(), Path::new("/tmp/x.zazz")).is_ok());
        assert!(output_path(t.path(), Path::new("/etc/x.zazz")).unwrap_err().contains("refusing"));
        assert!(output_path(t.path(), Path::new("missing/x.zazz")).is_err());
        assert!(output_path(t.path(), Path::new("/")).unwrap_err().contains("not a file name"));
    }

    // Checks atomic writes land and failures leave no temporary file.
    #[test]
    fn test_write_new() {
        let t = TempDir::new();
        let p = t.path().join("f");
        write_new(&p, b"x").expect("write");
        assert_eq!(std::fs::read(&p).expect("read"), b"x");
        assert!(write_new(&t.path().join("no/f"), b"x").is_err());
        let d = t.path().join("dir");
        std::fs::create_dir(&d).expect("mkdir");
        std::fs::write(d.join("keep"), "x").expect("keep");
        assert!(write_new(&d, b"x").is_err());
        assert_eq!(std::fs::read_dir(t.path()).expect("ls").count(), 2);
    }
}
