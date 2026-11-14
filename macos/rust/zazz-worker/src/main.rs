// zazz-worker: compiles and runs zazz WASM scripts for zazzled, inside the OS sandbox zazzled starts it in.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

mod engine;
mod net;
mod run;
mod serve;
mod testrun;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: zazz-worker compile < module.wasm > module.cwasm
       zazz-worker serve            (control socket on fd 3, precompiled module on fd 4)
       zazz-worker test MODULE.wasm --cwd DIR [--libtest] [--snapshots DIR] [--update] [--http-replay FILE] [--timeout-ms N] [--memory-mb N]
       zazz-worker version";

// Reads standard input, compiles it and writes the precompiled module to standard output.
fn compile_cmd(input: &mut impl Read, output: &mut impl Write) -> Result<(), String> {
    let mut wasm = Vec::new();
    input.read_to_end(&mut wasm).map_err(|e| format!("reading the module: {e}"))?;
    let cwasm = engine::compile(&wasm)?;
    output.write_all(&cwasm).map_err(|e| format!("writing the compiled module: {e}"))
}

// Parses the test command's options.
fn test_opts(args: &[String]) -> Result<(PathBuf, testrun::Opts), String> {
    let (mut module, mut cwd, mut snapshots, mut replay) = (None, None, None, None);
    let mut opts = (false, false, 120_000u64, 512u64);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--libtest" => opts.0 = true,
            "--update" => opts.1 = true,
            "--cwd" => cwd = Some(PathBuf::from(value()?)),
            "--snapshots" => snapshots = Some(PathBuf::from(value()?)),
            "--http-replay" => replay = Some(PathBuf::from(value()?)),
            "--timeout-ms" => opts.2 = value()?.parse().map_err(|_| "--timeout-ms needs a number")?,
            "--memory-mb" => opts.3 = value()?.parse().map_err(|_| "--memory-mb needs a number")?,
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => module = Some(PathBuf::from(s)),
        }
    }
    let replay = match replay {
        Some(p) => {
            let text = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            net::Replay::parse(
                &zazz::Json::parse_bytes(&text).map_err(|e| format!("{}: {}", p.display(), e.message))?,
            )?
        }
        None => net::Replay(Vec::new()),
    };
    let o = testrun::Opts {
        libtest: opts.0,
        cwd: cwd.ok_or("--cwd is required")?,
        snapshots,
        update: opts.1,
        replay,
        timeout_ms: opts.2,
        memory_mb: opts.3,
    };
    Ok((module.ok_or(USAGE)?, o))
}

// Compiles a test module and runs it, returning the JSON report.
fn test_cmd(args: &[String]) -> Result<String, String> {
    let (path, opts) = test_opts(args)?;
    let wasm = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let module = engine::load(&engine::compile(&wasm)?)?;
    testrun::run_tests(&module, opts).map(|r| r.to_string())
}

// Runs a subcommand and returns what to print.
fn dispatch(args: &[String]) -> Result<Option<String>, String> {
    match args.first().map(String::as_str) {
        Some("compile") => compile_cmd(&mut std::io::stdin(), &mut std::io::stdout()).map(|()| None),
        Some("serve") => serve::main().map(|()| None),
        Some("test") => test_cmd(&args[1..]).map(Some),
        Some("version") => Ok(Some(format!("zazz-worker {}", env!("CARGO_PKG_VERSION")))),
        _ => Err(USAGE.into()),
    }
}

// Entry point: errors go to stderr with exit status 1.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(Some(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zazz-worker: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Turns string slices into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks compile reads WASM from a reader and writes a module that loads.
    #[test]
    fn test_compile_cmd() {
        let wasm =
            wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).expect("wat");
        let mut out = Vec::new();
        compile_cmd(&mut &wasm[..], &mut out).expect("compile");
        assert!(engine::load(&out).is_ok());
        assert!(compile_cmd(&mut &b"x"[..], &mut Vec::new()).is_err());
    }

    // Checks option parsing for the test command.
    #[test]
    fn test_test_opts() {
        let (m, o) = test_opts(&v(&[
            "m.wasm",
            "--cwd",
            "/tmp",
            "--libtest",
            "--update",
            "--snapshots",
            "/s",
            "--timeout-ms",
            "5",
            "--memory-mb",
            "9",
        ]))
        .expect("opts");
        assert_eq!(
            (m, o.libtest, o.update, o.timeout_ms, o.memory_mb),
            (PathBuf::from("m.wasm"), true, true, 5, 9)
        );
        assert!(test_opts(&v(&["m.wasm"])).unwrap_err().contains("--cwd"));
        assert!(test_opts(&v(&["--cwd", "/tmp"])).unwrap_err().starts_with("usage"));
        assert!(test_opts(&v(&["--bogus"])).unwrap_err().contains("unknown option"));
        assert!(test_opts(&v(&["--cwd"])).unwrap_err().contains("needs a value"));
        assert!(test_opts(&v(&["--timeout-ms", "x"])).is_err());
        assert!(test_opts(&v(&["--memory-mb", "x"])).is_err());
        assert!(test_opts(&v(&["--http-replay", "/nonexistent"])).is_err());
        let replay = std::env::temp_dir().join(format!("zazz-replay-{}.json", std::process::id()));
        std::fs::write(&replay, r#"[{"request":{"url":"https://a.com/"},"response":{"text":"x"}}]"#)
            .expect("write");
        let (_, o) = test_opts(&v(&["m", "--cwd", "/tmp", "--http-replay", replay.to_str().expect("utf8")]))
            .expect("replay");
        assert_eq!(o.replay.0.len(), 1);
        std::fs::write(&replay, "{").expect("write");
        assert!(
            test_opts(&v(&["m", "--cwd", "/tmp", "--http-replay", replay.to_str().expect("utf8")])).is_err()
        );
        let _ = std::fs::remove_file(replay);
    }

    // Checks dispatch for version, usage and test errors.
    #[test]
    fn test_dispatch() {
        assert!(dispatch(&v(&["version"])).expect("version").expect("text").starts_with("zazz-worker "));
        assert!(dispatch(&v(&[])).unwrap_err().starts_with("usage"));
        assert!(dispatch(&v(&["test", "/nonexistent.wasm", "--cwd", "/tmp"])).is_err());
    }
}
