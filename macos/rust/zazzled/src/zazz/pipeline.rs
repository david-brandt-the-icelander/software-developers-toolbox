// The shared build-and-test pipeline behind `zazz add` and `zazz test`: extract, compile, AOT, run the tests.
use super::compile::{self, Failure};
use super::registry::Registry;
use super::{proc, sdk, tools};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use zazz_format::manifest::{Kind, Manifest};
use zazz_format::zip::Archive;

const FIXTURES: &str = "tests/fixtures";
const REPLAY: &str = "tests/http.json";
const SNAPSHOTS: &str = "tests/snapshots";
const TEST_MEMORY_MB: u64 = 512;

// A structured error reply: {"code", "message", ...extra}.
pub fn err(code: &str, message: &str) -> Value {
    json!({"code": code, "message": message})
}

// Where snapshot files live for a test run, and whether they may be rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshots {
    pub dir: PathBuf,
    pub update: bool,
}

// What the pipeline produced.
#[derive(Debug)]
pub struct Built {
    pub wasm: Vec<u8>,
    pub cwasm: Vec<u8>,
    pub tests: Option<Value>,
    pub warnings: Vec<String>,
}

// Finds rustc with the WASM target, or explains how to install it.
fn toolchain() -> Result<(PathBuf, String), Value> {
    let install = "install Rust with rustup, then run `rustup target add wasm32-wasip1` (or ship a prebuilt .wasm with kind \"wasm\")";
    let rc = tools::rustc().ok_or_else(|| err("no_toolchain", &format!("no rustc found; {install}")))?;
    let version = tools::rustc_version(&rc).map_err(|e| err("no_toolchain", &e))?;
    if !tools::has_target(&rc, tools::WASM_TARGET) {
        return Err(err(
            "no_toolchain",
            &format!("rustc has no {} standard library; {install}", tools::WASM_TARGET),
        ));
    }
    Ok((rc, version))
}

// Turns a build failure into an error reply.
fn build_error(f: Failure) -> Value {
    match f {
        Failure::Diagnostics(d) => {
            json!({"code": "build_failed", "message": format!("{} compile error(s)", d.len()), "diagnostics": d})
        }
        Failure::Message(m) => err("build_failed", &m),
    }
}

// Compiles the script (and its tests) to WASM according to its kind.
fn to_wasm(
    reg: &Registry,
    a: &Archive,
    m: &Manifest,
    src: &Path,
) -> Result<(Vec<u8>, Option<Vec<u8>>), Value> {
    match m.kind {
        Kind::Wasm => {
            let tests =
                m.tests.as_ref().and_then(|t| t.module.as_ref()).and_then(|p| a.get(p)).map(<[u8]>::to_vec);
            Ok((a.get(&m.entry).expect("checked by the manifest").to_vec(), tests))
        }
        Kind::Rust => {
            let tc = toolchain()?;
            let key = sdk::key(&tc.1, tools::WASM_TARGET);
            let sdk_dir = sdk::ensure_rlib(&reg.root, &tc.0, &key, tools::WASM_TARGET)
                .map_err(|e| err("build_failed", &e))?;
            let p = compile::rust_to_wasm(src, m, a, &tc, &sdk_dir).map_err(build_error)?;
            Ok((p.wasm, p.tests))
        }
        Kind::NativeBin => {
            Err(err("unsupported", "kind \"native-bin\" is accepted only from the zazzle release authority"))
        }
    }
}

// Copies the archive's test fixtures into the test working directory.
fn fixtures(a: &Archive, cwd: &Path) -> Result<(), Value> {
    for e in a.under(FIXTURES) {
        let p = cwd.join(&e.name[FIXTURES.len() + 1..]);
        let io = |x: std::io::Error| err("build_failed", &format!("{}: {x}", e.name));
        std::fs::create_dir_all(p.parent().expect("fixture files have a parent")).map_err(io)?;
        std::fs::write(&p, &e.data).map_err(io)?;
    }
    Ok(())
}

// Runs the test module through zazz-worker and returns its report.
fn run_tests(
    worker: &Path,
    m: &Manifest,
    tests_wasm: &Path,
    scratch: &Path,
    src: &Path,
    snaps: &Option<Snapshots>,
) -> Result<Value, Value> {
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&cwd).map_err(|e| err("build_failed", &e.to_string()))?;
    let t = m.tests.as_ref().expect("only scripts with tests get here");
    let mut cmd = Command::new(worker);
    cmd.env_clear().arg("test").arg(tests_wasm).arg("--cwd").arg(&cwd);
    cmd.args([
        "--timeout-ms",
        &t.timeout_ms.to_string(),
        "--memory-mb",
        &m.memory_mb.max(TEST_MEMORY_MB).to_string(),
    ]);
    if m.kind == Kind::Rust {
        cmd.arg("--libtest");
    }
    let snap = snaps.clone().unwrap_or(Snapshots { dir: src.join(SNAPSHOTS), update: false });
    if snap.update {
        std::fs::create_dir_all(&snap.dir)
            .map_err(|e| err("build_failed", &format!("{}: {e}", snap.dir.display())))?;
        cmd.arg("--update");
    }
    if snap.dir.is_dir() {
        cmd.arg("--snapshots").arg(&snap.dir);
    }
    if src.join(REPLAY).is_file() {
        cmd.arg("--http-replay").arg(src.join(REPLAY));
    }
    let r = proc::run(&mut cmd, None, Duration::from_millis(t.timeout_ms) + Duration::from_secs(120))
        .map_err(|e| err("tests_failed", &e))?;
    if !r.ok() {
        return Err(err("tests_failed", &format!("the test run did not finish: {}", r.err_text())));
    }
    serde_json::from_slice(&r.stdout)
        .map_err(|e| err("tests_failed", &format!("unreadable test report: {e}")))
}

// Builds a script from its archive, AOT-compiles it, and runs its tests; nothing is published here.
pub fn build_and_test(
    reg: &Registry,
    worker: &Path,
    a: &Archive,
    m: &Manifest,
    scratch: &Path,
    snaps: Option<Snapshots>,
) -> Result<Built, Value> {
    let src = scratch.join("src");
    compile::extract(a, &src).map_err(|e| err("invalid_archive", &e))?;
    let (wasm, tests_wasm) = to_wasm(reg, a, m, &src)?;
    let cwasm = compile::aot(worker, &wasm).map_err(|e| err("invalid_wasm", &e))?;
    let mut warnings = Vec::new();
    let tests = match tests_wasm {
        Some(t) => {
            let path = scratch.join("tests.wasm");
            std::fs::write(&path, &t).map_err(|e| err("build_failed", &e.to_string()))?;
            fixtures(a, &scratch.join("cwd"))?;
            let report = run_tests(worker, m, &path, scratch, &src, &snaps)?;
            if report["failed"].as_array().is_some_and(|f| !f.is_empty()) {
                let n = report["failed"].as_array().map_or(0, Vec::len);
                return Err(
                    json!({"code": "tests_failed", "message": format!("{n} test(s) failed"), "tests": report}),
                );
            }
            Some(report)
        }
        None => {
            warnings.push(
                "the script has no tests; add tests/*.rs that use zazz::test (see `zazz new`)".to_string(),
            );
            None
        }
    };
    Ok(Built { wasm, cwasm, tests, warnings })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{worker_bin, TempDir};
    use zazz_format::zip::Entry;

    // Builds an archive from names and texts.
    fn archive(files: &[(&str, &[u8])]) -> Archive {
        Archive::new(files.iter().map(|(n, d)| Entry { name: n.to_string(), data: d.to_vec() }).collect())
    }

    // Checks a native-bin script is refused and a prebuilt WASM without tests builds with a warning.
    #[test]
    fn test_kinds() {
        let t = TempDir::new();
        let reg = Registry::open(&t.path().join("reg")).expect("reg");
        let bin = Manifest::parse(br#"{"zazz":1,"name":"b","kind":"native-bin","entry":"b"}"#).expect("m");
        let a = archive(&[("b", b"x")]);
        let e =
            build_and_test(&reg, &worker_bin(), &a, &bin, &reg.scratch("b").expect("s"), None).unwrap_err();
        assert_eq!(e["code"], "unsupported");
        let wasm = wat_command();
        let m = Manifest::parse(br#"{"zazz":1,"name":"w","kind":"wasm","entry":"w.wasm"}"#).expect("m");
        let built = build_and_test(
            &reg,
            &worker_bin(),
            &archive(&[("w.wasm", &wasm)]),
            &m,
            &reg.scratch("w").expect("s"),
            None,
        )
        .expect("built");
        assert!(built.tests.is_none() && built.warnings.len() == 1 && !built.cwasm.is_empty());
        let bad = archive(&[("w.wasm", b"not wasm")]);
        assert_eq!(
            build_and_test(&reg, &worker_bin(), &bad, &m, &reg.scratch("x").expect("s"), None).unwrap_err()
                ["code"],
            "invalid_wasm"
        );
    }

    // Checks command-style WASM tests: fixtures are copied and failures stop the pipeline.
    #[test]
    fn test_wasm_command_tests() {
        let t = TempDir::new();
        let reg = Registry::open(&t.path().join("reg")).expect("reg");
        let m = Manifest::parse(
            br#"{"zazz":1,"name":"w","kind":"wasm","entry":"w.wasm","tests":{"module":"t.wasm"}}"#,
        )
        .expect("m");
        let ok = archive(&[
            ("w.wasm", &wat_command()),
            ("t.wasm", &wat_command()),
            ("tests/fixtures/d/a.txt", b"x"),
            ("tests/http.json", b"[]"),
        ]);
        let s = reg.scratch("ok").expect("s");
        let snaps = Some(Snapshots { dir: t.path().join("snaps"), update: true });
        let built = build_and_test(&reg, &worker_bin(), &ok, &m, &s, snaps).expect("built");
        assert_eq!(built.tests.expect("report")["passed"], 1);
        assert!(s.join("cwd/d/a.txt").is_file());
        let fail = archive(&[
            ("w.wasm", &wat_command()),
            (
                "t.wasm",
                &wat::parse_str(
                    r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#,
                )
                .expect("wat"),
            ),
        ]);
        let e =
            build_and_test(&reg, &worker_bin(), &fail, &m, &reg.scratch("f").expect("s"), None).unwrap_err();
        assert_eq!((e["code"].as_str(), e["tests"]["passed"].as_i64()), (Some("tests_failed"), Some(0)));
    }

    // A trivial WASI command.
    fn wat_command() -> Vec<u8> {
        wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).expect("wat")
    }

    // Checks error replies and build failures are shaped for agents.
    #[test]
    fn test_errors() {
        assert_eq!(err("a", "b"), json!({"code": "a", "message": "b"}));
        let d = build_error(Failure::Diagnostics(vec![json!({"msg": "x"})]));
        assert_eq!(
            (d["code"].as_str(), d["message"].as_str()),
            (Some("build_failed"), Some("1 compile error(s)"))
        );
        assert_eq!(build_error(Failure::Message("m".into()))["message"], "m");
    }
}
