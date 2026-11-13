// Builds scripts: extracts the archive, compiles Rust to WASM (script and tests), condenses diagnostics, and AOT-compiles WASM.
use super::{proc, tools};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use zazz_format::manifest::Manifest;
use zazz_format::zip::Archive;

const RUSTC_TIMEOUT: Duration = Duration::from_secs(300);
const AOT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_DIAGNOSTICS: usize = 20;
pub const TEST_ROOT: &str = "__zazz_tests.rs";

// Why a build failed: compiler diagnostics, or a single message.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    Diagnostics(Vec<Value>),
    Message(String),
}

// What a Rust build produced: the script module and, when the script has tests, the test module.
#[derive(Debug)]
pub struct Products {
    pub wasm: Vec<u8>,
    pub tests: Option<Vec<u8>>,
}

// Writes every archive entry under a fresh directory, refusing to follow or replace anything.
pub fn extract(a: &Archive, dir: &Path) -> Result<(), String> {
    for e in &a.entries {
        let p = dir.join(&e.name);
        std::fs::create_dir_all(p.parent().expect("entries are relative files"))
            .map_err(|err| format!("{}: {err}", e.name))?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&p)
            .map_err(|err| format!("{}: {err}", e.name))?;
        f.write_all(&e.data).map_err(|err| format!("{}: {err}", e.name))?;
    }
    Ok(())
}

// Resolves "." and ".." in a relative path, for readable diagnostics.
pub fn tidy_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "." | "" => {}
            ".." if parts.last().is_some_and(|l| *l != "..") => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts.join("/")
}

// Turns rustc's JSON diagnostics into short {level, file, line, col, msg, hint} records, errors only.
pub fn diagnostics(stderr: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in stderr.lines().filter(|l| l.starts_with('{')) {
        let Ok(d) = serde_json::from_str::<Value>(line) else { continue };
        let level = d["level"].as_str().unwrap_or("");
        let msg = d["message"].as_str().unwrap_or("");
        if d["$message_type"] != "diagnostic"
            || !level.starts_with("error")
            || msg.starts_with("aborting due to")
        {
            continue;
        }
        let span =
            d["spans"].as_array().and_then(|s| s.iter().find(|s| s["is_primary"] == true).or(s.first()));
        let hint = d["children"]
            .as_array()
            .and_then(|c| c.iter().find(|c| c["level"] == "help"))
            .and_then(|c| c["message"].as_str());
        out.push(json!({
            "level": level,
            "file": span.and_then(|s| s["file_name"].as_str()).map(tidy_path),
            "line": span.and_then(|s| s["line_start"].as_u64()),
            "col": span.and_then(|s| s["column_start"].as_u64()),
            "msg": msg,
            "hint": hint,
        }));
        if out.len() == MAX_DIAGNOSTICS {
            break;
        }
    }
    out
}

// Lists the test source files: each tests path that is a .rs file, or the .rs files directly inside it.
pub fn test_files(m: &Manifest, a: &Archive) -> Vec<String> {
    let Some(t) = &m.tests else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for p in &t.paths {
        if a.get(p).is_some() && p.ends_with(".rs") {
            out.push(p.clone());
            continue;
        }
        let direct = a.under(p).filter(|e| e.name.ends_with(".rs") && !e.name[p.len() + 1..].contains('/'));
        out.extend(direct.map(|e| e.name.clone()));
    }
    out.sort();
    out.dedup();
    out
}

// Builds the generated test crate root: the script as module `script`, each test file as its own module.
pub fn test_root(entry: &str, files: &[String]) -> (String, String) {
    let (dir, file) = entry.rsplit_once('/').unwrap_or(("", entry));
    let up = "../".repeat(dir.split('/').filter(|c| !c.is_empty()).count());
    let mut text = format!("#![allow(dead_code, unused_imports)]\n#[path = \"{file}\"]\nmod script;\n");
    let mut used: Vec<String> = Vec::new();
    for f in files {
        let stem = f.rsplit('/').next().unwrap_or(f).trim_end_matches(".rs");
        let mut name: String = stem
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
            .collect();
        if name.is_empty()
            || name.starts_with(|c: char| c.is_ascii_digit())
            || name == "script"
            || used.contains(&name)
        {
            name = format!("t{}_{name}", used.len());
        }
        used.push(name.clone());
        text.push_str(&format!("#[path = \"{up}{f}\"]\nmod {name};\n"));
    }
    let root = if dir.is_empty() { TEST_ROOT.to_string() } else { format!("{dir}/{TEST_ROOT}") };
    (root, text)
}

// Runs rustc in the build directory and returns the output file, or the diagnostics.
fn rustc(
    tc: &(PathBuf, String),
    dir: &Path,
    sdk: &Path,
    edition: &str,
    args: &[&str],
    out: &str,
) -> Result<Vec<u8>, Failure> {
    let extern_arg = format!("zazz={}", sdk.join("libzazz.rlib").display());
    let dep = format!("dependency={}", sdk.display());
    let mut cmd = Command::new(&tc.0);
    tools::rustc_env(&mut cmd)
        .current_dir(dir)
        .args([
            "--edition",
            edition,
            "--target",
            tools::WASM_TARGET,
            "--error-format=json",
            "-C",
            "debuginfo=0",
        ])
        .args(["-L", &dep, "--extern", &extern_arg, "-o", out])
        .args(args);
    let r = proc::run(&mut cmd, None, RUSTC_TIMEOUT).map_err(Failure::Message)?;
    if r.timed_out {
        return Err(Failure::Message("rustc ran past its time limit".into()));
    }
    if !r.ok() {
        let d = diagnostics(&String::from_utf8_lossy(&r.stderr));
        return Err(if d.is_empty() {
            Failure::Message(format!("rustc failed: {}", r.err_text()))
        } else {
            Failure::Diagnostics(d)
        });
    }
    std::fs::read(dir.join(out)).map_err(|e| Failure::Message(format!("{out}: {e}")))
}

// Compiles a Rust script and its tests to WASM in an extracted build directory.
pub fn rust_to_wasm(
    dir: &Path,
    m: &Manifest,
    a: &Archive,
    tc: &(PathBuf, String),
    sdk: &Path,
) -> Result<Products, Failure> {
    let wasm = rustc(
        tc,
        dir,
        sdk,
        &m.edition,
        &["--crate-type", "bin", "--crate-name", "script", "-C", "opt-level=s", &m.entry],
        "script.wasm",
    )?;
    let files = test_files(m, a);
    if files.is_empty() {
        return Ok(Products { wasm, tests: None });
    }
    let (root, text) = test_root(&m.entry, &files);
    std::fs::write(dir.join(&root), text).map_err(|e| Failure::Message(format!("{root}: {e}")))?;
    let tests = rustc(
        tc,
        dir,
        sdk,
        &m.edition,
        &["--test", "--crate-name", "script_tests", "-C", "opt-level=1", &root],
        "tests.wasm",
    )?;
    Ok(Products { wasm, tests: Some(tests) })
}

// Ahead-of-time compiles a WASM module with zazz-worker, which also checks its imports.
pub fn aot(worker: &Path, wasm: &[u8]) -> Result<Vec<u8>, String> {
    let r = proc::run(Command::new(worker).arg("compile").env_clear(), Some(wasm.to_vec()), AOT_TIMEOUT)?;
    if !r.ok() {
        let msg = r.err_text();
        return Err(msg.strip_prefix("zazz-worker: ").unwrap_or(&msg).to_string());
    }
    Ok(r.stdout)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use zazz_format::zip::Entry;

    // Builds an archive from names and contents.
    fn archive(files: &[(&str, &str)]) -> Archive {
        Archive::new(
            files.iter().map(|(n, d)| Entry { name: n.to_string(), data: d.as_bytes().to_vec() }).collect(),
        )
    }

    // Checks extraction writes files and refuses to overwrite.
    #[test]
    fn test_extract() {
        let t = TempDir::new();
        let a = archive(&[("src/main.rs", "fn main() {}"), ("zazz.json", "{}")]);
        extract(&a, t.path()).expect("extract");
        assert_eq!(std::fs::read_to_string(t.path().join("src/main.rs")).expect("read"), "fn main() {}");
        assert!(extract(&a, t.path()).is_err());
        std::fs::write(t.path().join("blocker"), "").expect("file");
        assert!(extract(&archive(&[("blocker/x", "")]), t.path()).is_err());
    }

    // Checks diagnostics keep errors with location and help, and skip the rest.
    #[test]
    fn test_diagnostics() {
        let err = json!({"$message_type": "diagnostic", "level": "error", "message": "cannot find value `x`",
            "spans": [{"file_name": "src/../tests/a.rs", "line_start": 3, "column_start": 5, "is_primary": true}],
            "children": [{"level": "help", "message": "a local variable with a similar name exists: `y`"}]});
        let warn = json!({"$message_type": "diagnostic", "level": "warning", "message": "unused", "spans": [], "children": []});
        let abort = json!({"$message_type": "diagnostic", "level": "error", "message": "aborting due to 1 previous error", "spans": [], "children": []});
        let nospan = json!({"$message_type": "diagnostic", "level": "error", "message": "no main", "spans": [], "children": []});
        let text = format!("{err}\n{warn}\nnot json\n{{bad\n{abort}\n{nospan}\n");
        let d = diagnostics(&text);
        assert_eq!(d.len(), 2);
        assert_eq!(
            d[0],
            json!({"level": "error", "file": "tests/a.rs", "line": 3, "col": 5, "msg": "cannot find value `x`", "hint": "a local variable with a similar name exists: `y`"})
        );
        assert_eq!(d[1]["file"], Value::Null);
        let many = format!("{err}\n").repeat(30);
        assert_eq!(diagnostics(&many).len(), MAX_DIAGNOSTICS);
        assert_eq!((tidy_path("./a/../../b"), tidy_path("a//b/.")), ("../b".to_string(), "a/b".to_string()));
    }

    // Checks test files are found from directories and files, top level only.
    #[test]
    fn test_test_files_and_root() {
        let m = Manifest::parse(br#"{"zazz":1,"name":"x","kind":"rust","entry":"src/main.rs","tests":{"paths":["tests","extra/one.rs"]}}"#).expect("m");
        let a = archive(&[
            ("src/main.rs", ""),
            ("tests/b.rs", ""),
            ("tests/a-1.rs", ""),
            ("tests/fixtures/x.rs", ""),
            ("tests/x.txt", ""),
            ("extra/one.rs", ""),
        ]);
        let files = test_files(&m, &a);
        assert_eq!(files, vec!["extra/one.rs", "tests/a-1.rs", "tests/b.rs"]);
        let (root, text) = test_root("src/main.rs", &files);
        assert_eq!(root, "src/__zazz_tests.rs");
        assert!(text.contains("#[path = \"main.rs\"]\nmod script;"));
        assert!(text.contains("#[path = \"../extra/one.rs\"]\nmod one;"));
        assert!(text.contains("mod a_1;") && text.contains("mod b;"));
        let (root, text) = test_root("main.rs", &["t/script.rs".into(), "u/script.rs".into(), "1.rs".into()]);
        assert_eq!(root, TEST_ROOT);
        assert!(
            text.contains("#[path = \"t/script.rs\"]\nmod t0_script;")
                && text.contains("mod t1_script;")
                && text.contains("mod t2_1;")
        );
        let none =
            Manifest::parse(br#"{"zazz":1,"name":"x","kind":"rust","entry":"src/main.rs"}"#).expect("m");
        assert!(test_files(&none, &a).is_empty());
    }

    // Checks AOT failures report the worker's message, and a missing worker is an error.
    #[test]
    fn test_aot_errors() {
        let t = TempDir::new();
        let fake = t.write("w", b"#!/bin/sh\necho 'zazz-worker: invalid WebAssembly: x' >&2\nexit 1\n");
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        assert_eq!(aot(&fake, b"x").unwrap_err(), "invalid WebAssembly: x");
        assert!(aot(&t.path().join("missing"), b"x").is_err());
    }
}
