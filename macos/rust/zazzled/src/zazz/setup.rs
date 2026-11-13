// zazz new and zazz doctor: a script template to start from, and a check of everything zazz needs.
use super::pipeline::err;
use super::{install, sandbox, tools, Zazz};
use serde_json::{json, Value};
use std::path::Path;

const MAIN: &str = r#"// NAME: counts the lines and words of files (a starting point; edit freely).
use zazz::{json, Call, Error, Handlers, Json, Result};

// Counts lines and words in each file (or standard input for "-").
pub fn run(call: &mut Call, input: Json) -> Result<Json> {
    let mut rows = Vec::new();
    for f in input.strs("files")? {
        let text = String::from_utf8(call.read_input(&f)?).map_err(|_| Error::bad_input(&format!("{f} is not UTF-8")))?;
        rows.push(json!({"file": (f), "lines": (text.lines().count()), "words": (text.split_whitespace().count())}));
    }
    Ok(Json::Arr(rows))
}

// The script's handlers; tests use the same function.
pub fn handlers() -> Handlers {
    Handlers::new().json_command(run)
}

// Serves calls from zazz.
fn main() {
    zazz::serve(handlers());
}
"#;

const TEST: &str = r#"// Tests for NAME, run by `zazz test` with fakes for the filesystem and network.
use crate::script::handlers;
use zazz::json;
use zazz::test::*;

// Counts a file from the fake filesystem.
#[test]
fn counts_a_file() {
    let mut h = Harness::from_manifest(handlers(), include_str!("../zazz.json"));
    h.fs().add("a.txt", "one two\nthree\n");
    let out = h.json_command(json!({"files": ["a.txt"]})).unwrap();
    assert_json_eq!(out, json!([{"file": "a.txt", "lines": 2, "words": 3}]));
}

// Reads standard input for "-", and parses arguments as a human run would.
#[test]
fn counts_stdin_for_humans() {
    let mut h = Harness::from_manifest(handlers(), include_str!("../zazz.json"));
    h.stdin("a b c\n");
    let o = h.human(&["-"]);
    assert_eq!((o.exit, o.stdout.as_str()), (0, "file\tlines\twords\n-\t1\t3\n"));
}
"#;

// The manifest for a new Rust script.
fn manifest(name: &str) -> Value {
    json!({
        "zazz": 1, "name": name, "version": "0.1.0", "description": "Counts lines and words.",
        "kind": "rust", "entry": "src/main.rs", "tests": {"paths": ["tests"]},
        "sandbox": {"read": ["$CWD"], "write": [], "network": "none"},
        "cli": {"usage": format!("{name} FILE..."), "args": [{"name": "files", "positional": true, "type": "path", "multiple": true, "required": true, "help": "files to count (- for stdin)"}]}
    })
}

// Runs `zazz new NAME`: writes NAME/ with a manifest, src/main.rs and tests/basic.rs.
pub fn new(cwd: &Path, args: &[String]) -> Result<Value, Value> {
    let [name] = args else { return Err(err("usage", "usage: zazz new NAME")) };
    install::check_name(name).map_err(|e| err("bad_input", &e))?;
    let dir = cwd.join(name);
    if dir.exists() {
        return Err(err("exists", &format!("{} already exists", dir.display())));
    }
    let files = [
        ("zazz.json", serde_json::to_string_pretty(&manifest(name)).unwrap_or_default() + "\n"),
        ("src/main.rs", MAIN.replace("NAME", name)),
        ("tests/basic.rs", TEST.replace("NAME", name)),
    ];
    for (rel, text) in &files {
        let p = dir.join(rel);
        let io = |e: std::io::Error| err("io", &format!("{}: {e}", p.display()));
        std::fs::create_dir_all(p.parent().expect("template files have a parent")).map_err(io)?;
        std::fs::write(&p, text).map_err(io)?;
    }
    Ok(json!({"dir": dir.to_string_lossy(), "files": files.iter().map(|f| f.0).collect::<Vec<_>>(),
        "next": format!("edit {name}/src/main.rs and {name}/tests/basic.rs, then `zazz test {name}`, `zazz pack {name}`, `zazz add {name}.zazz`")}))
}

// One doctor check: ok, or the problem and how to fix it.
fn check(ok: bool, what: Value, fix: &str) -> Value {
    if ok {
        json!({"ok": true, "found": what})
    } else {
        json!({"ok": false, "found": what, "fix": fix})
    }
}

// Runs `zazz doctor`: the toolchain, WASM target, worker, sandbox, registry and shim directory.
pub fn doctor(z: &Zazz) -> Value {
    let rustc = tools::rustc();
    let version = rustc.as_ref().and_then(|r| tools::rustc_version(r).ok());
    let target = rustc.as_ref().is_some_and(|r| tools::has_target(r, tools::WASM_TARGET));
    let worker = z.worker().ok();
    let bin = z.bin_dir();
    let on_path = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == bin));
    let path_of = |p: &Option<std::path::PathBuf>| json!(p.as_ref().map(|p| p.to_string_lossy().into_owned()));
    json!({
        "rustc": check(version.is_some(), json!(version.as_deref().and_then(|v| v.lines().next())), "install Rust with rustup (only needed for kind \"rust\" scripts)"),
        "wasm_target": check(target, json!(tools::WASM_TARGET), "rustup target add wasm32-wasip1"),
        "worker": check(worker.is_some(), path_of(&worker), "install zazz-worker beside zazzled (it ships in the release tarball)"),
        "sandbox": check(sandbox::available().is_ok(), json!(sandbox::available().err()), "macOS: /usr/bin/sandbox-exec; Linux: a kernel with Landlock enabled"),
        "registry": check(z.registry().is_ok(), json!(z.registry().map(|r| r.root.to_string_lossy().into_owned()).unwrap_or_else(|e| e)), "make ~/.zazzle writable"),
        "invoker": check(z.invoker().is_some(), path_of(&z.invoker()), "install zazzle (and the zazz symlink) beside zazzled"),
        "shims_on_path": check(on_path, json!(bin.to_string_lossy()), &format!("export PATH=\"{}:$PATH\"", bin.display())),
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{run_cmd_with, zazz_engine, TempDir};

    // Checks the template is created, refuses to overwrite, tests green, and the doctor reports each check.
    #[test]
    fn test_new_template_passes_its_tests() {
        let t = TempDir::new();
        let e = zazz_engine(&t);
        let run = |args: &[&str]| serde_json::from_str::<Value>(&run_cmd_with(&e, t.path(), "", args).text).expect("json");
        let made = run(&["zazz", "new", "wc"]);
        assert_eq!(made["files"], json!(["zazz.json", "src/main.rs", "tests/basic.rs"]));
        assert_eq!(run(&["zazz", "new", "wc"])["error"]["code"], "exists");
        assert_eq!(run(&["zazz", "new", "Bad"])["error"]["code"], "bad_input");
        assert_eq!(run(&["zazz", "new"])["error"]["code"], "usage");
        let tested = run(&["zazz", "test", "wc"]);
        assert_eq!(tested["tests"]["passed"], 2, "{tested}");
        let d = run(&["zazz", "doctor"]);
        for k in ["rustc", "wasm_target", "worker", "sandbox", "registry"] {
            assert_eq!(d[k]["ok"], true, "{k}: {d}");
        }
        assert_eq!(d["invoker"]["ok"], false);
        assert!(d["invoker"]["fix"].is_string());
    }
}
