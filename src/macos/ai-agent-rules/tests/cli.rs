// Runs the built ai-agent-rules binary to check its output, exit codes and argument handling end to end.
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Output};

// Runs the binary with the given arguments and an empty home folder.
fn run(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ai-agent-rules"))
        .env("HOME", "/nonexistent")
        .args(args)
        .output()
        .expect("binary runs")
}

// A successful run prints to standard output and exits 0.
#[test]
fn test_cli_prints_version_and_names() {
    let out = run(&["--version".as_ref()]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ai-agent-rules "));
    let out = run(&["get-rules".as_ref(), env!("CARGO_MANIFEST_DIR").as_ref(), "--name-only".as_ref()]);
    assert_eq!(out.status.code(), Some(0));
}

// Errors go to standard error with their exit code.
#[test]
fn test_cli_errors_use_stderr() {
    let out = run(&["get-rules".as_ref(), "/no/such/path".as_ref()]);
    assert_eq!(out.status.code(), Some(66));
    assert!(out.stdout.is_empty() && !out.stderr.is_empty());
    assert_eq!(run(&[]).status.code(), Some(2));
}

// An argument that is not valid UTF-8 is a usage error, not a panic.
#[test]
fn test_cli_rejects_non_utf8_argument() {
    let out = run(&["get-rules".as_ref(), std::ffi::OsStr::from_bytes(&[0xff, 0xfe])]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("UTF-8"));
}

// check prints its problems to standard output with exit 1, so they can be piped.
#[test]
fn test_cli_check_problems_on_stdout() {
    let dir = std::env::temp_dir().join(format!("zalr-cli-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("folder");
    std::fs::write(dir.join("AGENTS.md"), "@gone.md\n").expect("writable");
    let out = run(&["check".as_ref(), dir.as_os_str()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("AGENTS.md:1: @gone.md points to no file"));
    assert!(out.stderr.is_empty());
}
