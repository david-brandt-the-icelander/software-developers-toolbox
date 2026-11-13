// Native code for scripts whose signer is trusted for it: a host build next to the WASM one, its tests, and how it runs.
use super::compile::{self, Failure};
use super::pipeline::err;
use super::registry::Registry;
use super::sandbox::{self, Policy};
use super::{proc, sdk, tools};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zazz_format::manifest::Manifest;
use zazz_format::zip::Archive;

const MAX_FAILURES: usize = 20;

// Parses libtest's plain output into {passed, ignored, failed: [{name, message, file, line}]}.
pub fn parse_libtest(stdout: &str) -> Value {
    let (mut passed, mut ignored, mut failed) = (0, 0, Vec::new());
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix("test ") else { continue };
        if let Some(name) = rest.strip_suffix(" ... ok") {
            passed += !name.is_empty() as i64;
        } else if rest.ends_with(" ... ignored") || rest.contains(" ... ignored, ") {
            ignored += 1;
        } else if let Some(name) = rest.strip_suffix(" ... FAILED") {
            failed.push(failure(name, stdout));
        }
    }
    failed.truncate(MAX_FAILURES);
    json!({"passed": passed, "ignored": ignored, "failed": failed})
}

// Pulls one failed test's panic message and location out of the output's failure sections.
fn failure(name: &str, stdout: &str) -> Value {
    let header = format!("---- {name} stdout ----");
    let section: Vec<&str> = stdout
        .lines()
        .skip_while(|l| *l != header)
        .skip(1)
        .take_while(|l| !l.starts_with("---- ") && *l != "failures:")
        .collect();
    let at = section.iter().position(|l| l.contains("panicked at "));
    let (file, line, message) = match at {
        Some(i) => {
            let loc = section[i].split("panicked at ").nth(1).unwrap_or("").trim_end_matches(':');
            let mut parts = loc.split(':');
            let file = compile::tidy_path(parts.next().unwrap_or(""));
            let line = parts.next().and_then(|n| n.parse::<i64>().ok());
            let msg: Vec<&str> = section[i + 1..]
                .iter()
                .take_while(|l| !l.starts_with("note:") && !l.is_empty())
                .copied()
                .collect();
            (Some(file), line, msg.join("\n"))
        }
        None => (None, None, section.join("\n").trim().to_string()),
    };
    json!({"name": name, "message": message, "file": file, "line": line})
}

// Builds the host binary and its tests, runs the tests in the sandbox, and returns the binary.
pub fn build_and_test(
    reg: &Registry,
    a: &Archive,
    m: &Manifest,
    scratch: &Path,
) -> Result<(Vec<u8>, Option<Value>), Value> {
    let rc = tools::rustc().ok_or_else(|| err("no_toolchain", "a native build needs rustc"))?;
    let version = tools::rustc_version(&rc).map_err(|e| err("no_toolchain", &e))?;
    let host = tools::host_triple(&version).ok_or_else(|| err("no_toolchain", "rustc -vV names no host"))?;
    let key = sdk::key(&version, &host);
    let sdk_dir = sdk::ensure_rlib(&reg.root, &rc, &key, &host).map_err(|e| err("build_failed", &e))?;
    let src = scratch.join("native");
    compile::extract(a, &src).map_err(|e| err("invalid_archive", &e))?;
    let products = compile::rust_build(&src, m, a, &(rc, version), &sdk_dir, &host).map_err(|f| match f {
        Failure::Diagnostics(d) => {
            json!({"code": "build_failed", "message": "native build failed", "diagnostics": d})
        }
        Failure::Message(msg) => err("build_failed", &msg),
    })?;
    let Some(tests) = products.tests else { return Ok((products.wasm, None)) };
    let bin = scratch.join("native-tests");
    write_exe(&bin, &tests).map_err(|e| err("build_failed", &e))?;
    let cwd = scratch.join("native-cwd");
    std::fs::create_dir_all(&cwd).map_err(|e| err("build_failed", &e.to_string()))?;
    let snaps = src.join("tests/snapshots");
    let policy = Policy::for_program(&bin).writing([cwd.clone()]).reading([snaps.clone()]);
    let mut cmd = sandbox::command(&bin, &["--test-threads".into(), "1".into()], &policy)
        .map_err(|e| err("sandbox_unavailable", &e))?;
    cmd.env_clear().current_dir(&cwd).env("ZAZZ_SNAPSHOTS", &snaps).env("RUST_BACKTRACE", "0");
    let timeout = Duration::from_millis(m.tests.as_ref().map_or(120_000, |t| t.timeout_ms));
    let r = proc::run(&mut cmd, None, timeout).map_err(|e| err("tests_failed", &e))?;
    let report = parse_libtest(&String::from_utf8_lossy(&r.stdout));
    if r.timed_out || report["failed"].as_array().is_some_and(|f| !f.is_empty()) || !r.ok() {
        return Err(json!({"code": "tests_failed", "message": "native tests failed", "tests": report}));
    }
    Ok((products.wasm, Some(report)))
}

// Writes an executable file.
pub fn write_exe(path: &Path, data: &[u8]) -> Result<(), String> {
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .map_err(|e| e.to_string())
}

// The program and sandbox for a native worker: its own binary, the manifest's paths, the network only when granted.
pub fn worker(bin: &Path, mounts: &[Value], tmp: PathBuf, network: bool) -> (Vec<String>, Policy) {
    let host = |write: bool| {
        mounts
            .iter()
            .filter(|p| p["write"] == write)
            .filter_map(|p| p["host"].as_str().map(PathBuf::from))
            .collect::<Vec<_>>()
    };
    let mut policy =
        Policy::for_program(bin).reading(host(false)).writing(host(true).into_iter().chain([tmp]));
    policy.network = network;
    (vec![bin.to_string_lossy().into_owned()], policy)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks the libtest parser: passes, ignores and failures with their panic location.
    #[test]
    fn test_parse_libtest() {
        let out = "running 4 tests\ntest a ... ok\ntest b ... ignored\ntest c ... FAILED\ntest d ... ignored, slow\n\nfailures:\n\n---- c stdout ----\n\nthread 'c' panicked at src/../tests/t.rs:4:5:\nboom\nnote: run with RUST_BACKTRACE=1\n\nfailures:\n    c\n";
        let r = parse_libtest(out);
        assert_eq!((r["passed"].as_i64(), r["ignored"].as_i64()), (Some(1), Some(2)));
        assert_eq!(r["failed"][0], json!({"name": "c", "message": "boom", "file": "tests/t.rs", "line": 4}));
        let bare = parse_libtest("test x ... FAILED\n---- x stdout ----\nsomething broke\n");
        assert_eq!(bare["failed"][0]["message"], "something broke");
        let two = parse_libtest(
            "test x ... FAILED\ntest y ... FAILED\n---- x stdout ----\nfirst\n---- y stdout ----\nsecond\n",
        );
        assert_eq!(
            (two["failed"][0]["message"].as_str(), two["failed"][1]["message"].as_str()),
            (Some("first"), Some("second"))
        );
        assert_eq!(parse_libtest("noise\ntest  ... ok\n")["passed"], 0);
    }

    // Checks the native worker's program and sandbox.
    #[test]
    fn test_worker() {
        let t = crate::testutil::TempDir::new();
        let mounts = vec![json!({"host": t.path().to_string_lossy(), "write": false})];
        let (prog, p) = worker(Path::new("/bin/true"), &mounts, t.path().to_path_buf(), true);
        assert_eq!(prog, vec!["/bin/true".to_string()]);
        assert!(p.network && p.read.contains(&t.path().to_path_buf()));
        assert!(write_exe(&t.path().join("no/x"), b"").is_err());
    }
}
