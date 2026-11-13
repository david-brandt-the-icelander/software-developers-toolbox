// Finds the tools zazz needs at fixed places (never PATH, which launchd leaves empty): rustc and zazz-worker.
use super::proc;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const WASM_TARGET: &str = "wasm32-wasip1";
const PROBE: Duration = Duration::from_secs(30);

// Returns the first existing file among candidates.
fn first_file(candidates: Vec<Option<PathBuf>>) -> Option<PathBuf> {
    candidates.into_iter().flatten().find(|p| p.is_file())
}

// Finds rustc: $ZAZZ_RUSTC, ~/.cargo/bin/rustc, then the Homebrew and /usr/local locations.
pub fn rustc() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    first_file(vec![
        std::env::var_os("ZAZZ_RUSTC").map(PathBuf::from),
        home.map(|h| h.join(".cargo/bin/rustc")),
        Some("/opt/homebrew/bin/rustc".into()),
        Some("/usr/local/bin/rustc".into()),
    ])
}

// Finds zazz-worker: $ZAZZ_WORKER, else beside the running zazzled.
pub fn worker() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("zazz-worker")));
    first_file(vec![std::env::var_os("ZAZZ_WORKER").map(PathBuf::from), beside])
}

// The environment rustc runs with: only what rustup needs to find its toolchains.
pub fn rustc_env(cmd: &mut Command) -> &mut Command {
    cmd.env_clear();
    for k in ["HOME", "RUSTUP_HOME", "CARGO_HOME", "RUSTUP_TOOLCHAIN", "PATH"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    cmd
}

// Returns `rustc -vV`, which identifies the toolchain in build keys.
pub fn rustc_version(rustc: &Path) -> Result<String, String> {
    let r = proc::run(rustc_env(Command::new(rustc).arg("-vV")), None, PROBE)?;
    if !r.ok() {
        return Err(format!("{} -vV failed: {}", rustc.display(), r.err_text()));
    }
    Ok(String::from_utf8_lossy(&r.stdout).trim().to_string())
}

// True when the toolchain has the standard library for a target.
pub fn has_target(rustc: &Path, target: &str) -> bool {
    let Ok(r) = proc::run(rustc_env(Command::new(rustc).args(["--print", "sysroot"])), None, PROBE) else {
        return false;
    };
    let sysroot = PathBuf::from(String::from_utf8_lossy(&r.stdout).trim());
    r.ok() && sysroot.join("lib/rustlib").join(target).join("lib").is_dir()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};

    // Checks the environment overrides win and missing files are skipped.
    #[test]
    fn test_overrides() {
        let _g = env_lock();
        let t = TempDir::new();
        let fake = t.write("rustc", b"");
        std::env::set_var("ZAZZ_RUSTC", &fake);
        assert_eq!(rustc(), Some(fake.clone()));
        std::env::set_var("ZAZZ_WORKER", &fake);
        assert_eq!(worker(), Some(fake));
        std::env::set_var("ZAZZ_WORKER", t.path().join("missing"));
        assert_ne!(worker(), Some(t.path().join("missing")));
        std::env::remove_var("ZAZZ_RUSTC");
        std::env::remove_var("ZAZZ_WORKER");
        assert_eq!(first_file(vec![None, Some(t.path().join("nope"))]), None);
    }

    // Checks version and target probes against the real toolchain, and failures on a fake one.
    #[test]
    fn test_probes() {
        let rc = rustc().expect("a rustc for tests");
        assert!(rustc_version(&rc).expect("version").contains("host: "));
        assert!(has_target(&rc, WASM_TARGET), "install it: rustup target add {WASM_TARGET}");
        assert!(!has_target(&rc, "no-such-target"));
        let t = TempDir::new();
        let bad = t.write("rustc", b"#!/bin/sh\necho broken >&2\nexit 1\n");
        std::fs::set_permissions(&bad, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        assert!(rustc_version(&bad).unwrap_err().contains("broken"));
        assert!(!has_target(&bad, WASM_TARGET));
        assert!(rustc_version(&t.path().join("missing")).is_err());
        assert!(!has_target(&t.path().join("missing"), WASM_TARGET));
    }
}
