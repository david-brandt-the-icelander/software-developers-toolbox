// Finds the tools zazz needs at fixed places (never PATH, which launchd leaves empty): rustc and zazz-worker.
use super::proc;
use super::sandbox::{self, Policy};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const WASM_TARGET: &str = "wasm32-wasip1";
const PROBE: Duration = Duration::from_secs(30);

// Returns the first existing file among candidates.
fn first_file(candidates: Vec<Option<PathBuf>>) -> Option<PathBuf> {
    candidates.into_iter().flatten().find(|p| p.is_file())
}

// Finds rustc: $ZAZZ_RUSTC, $CARGO_HOME/bin/rustc, ~/.cargo/bin/rustc, then the Homebrew and /usr/local locations.
pub fn rustc() -> Option<PathBuf> {
    rustc_from(std::env::var_os("ZAZZ_RUSTC"), std::env::var_os("CARGO_HOME"), std::env::var_os("HOME"))
}

// Finds rustc from explicit environment values.
pub fn rustc_from(
    over: Option<OsString>,
    cargo_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    first_file(vec![
        over.map(PathBuf::from),
        cargo_home.map(|c| PathBuf::from(c).join("bin/rustc")),
        home.map(|h| PathBuf::from(h).join(".cargo/bin/rustc")),
        Some("/opt/homebrew/bin/rustc".into()),
        Some("/usr/local/bin/rustc".into()),
    ])
}

// Finds zazz-worker: $ZAZZ_WORKER, else beside the running zazzled.
pub fn worker() -> Option<PathBuf> {
    worker_from(std::env::var_os("ZAZZ_WORKER"))
}

// Finds zazz-worker from an explicit override.
pub fn worker_from(over: Option<OsString>) -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("zazz-worker")));
    first_file(vec![over.map(PathBuf::from), beside])
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

// Returns the toolchain's sysroot.
pub fn sysroot(rustc: &Path) -> Option<PathBuf> {
    let r = proc::run(rustc_env(Command::new(rustc).args(["--print", "sysroot"])), None, PROBE).ok()?;
    r.ok().then(|| PathBuf::from(String::from_utf8_lossy(&r.stdout).trim()))
}

// True when the toolchain has the standard library for a target.
pub fn has_target(rustc: &Path, target: &str) -> bool {
    sysroot(rustc).is_some_and(|s| s.join("lib/rustlib").join(target).join("lib").is_dir())
}

// A home-relative directory from an environment variable, else under HOME.
fn home_dir(var: &str, rel: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rel)))
}

// Native builds link with the system C toolchain: let rustc run it and read its files.
fn with_linker(p: Policy) -> Policy {
    let toolchain = [
        "/usr/bin",
        "/bin",
        "/usr/libexec",
        "/usr/lib",
        "/usr/local/bin",
        "/Library/Developer/CommandLineTools",
        "/Applications/Xcode.app",
    ];
    let reads = [
        "/usr/include",
        "/usr/local/lib",
        "/etc",
        "/private/var/db/xcode_select_link",
        "/private/var/select",
    ];
    p.executing(toolchain.iter().map(PathBuf::from))
        .reading(reads.iter().chain(toolchain.iter()).map(PathBuf::from))
}

// The host target triple from `rustc -vV`.
pub fn host_triple(version: &str) -> Option<String> {
    version.lines().find_map(|l| l.strip_prefix("host: ")).map(str::to_string)
}

// rustc inside the sandbox: it reads the toolchain, rustup's files and `reads`, executes the toolchain (and the linker for native builds), writes only `build`.
pub fn rustc_command(
    rustc: &Path,
    build: &Path,
    reads: &[PathBuf],
    args: &[OsString],
    native: bool,
) -> Result<Command, String> {
    let sysroot = sysroot(rustc).ok_or_else(|| format!("{} --print sysroot failed", rustc.display()))?;
    let (rustup, cargo) = (home_dir("RUSTUP_HOME", ".rustup"), home_dir("CARGO_HOME", ".cargo"));
    let policy = Policy::for_program(rustc)
        .reading(
            rustup.iter().chain(cargo.iter()).cloned().chain(reads.iter().cloned()).chain([sysroot.clone()]),
        )
        .executing([sysroot].into_iter().chain(cargo.map(|c| c.join("bin"))))
        .writing([build.to_path_buf()]);
    let policy = if native { with_linker(policy) } else { policy };
    let mut cmd = sandbox::command(rustc, args, &policy)?;
    rustc_env(&mut cmd).current_dir(build).env("TMPDIR", build);
    Ok(cmd)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks the overrides win and missing files are skipped.
    #[test]
    fn test_overrides() {
        let t = TempDir::new();
        let fake = t.write("rustc", b"");
        assert_eq!(rustc_from(Some(fake.clone().into()), None, None), Some(fake.clone()));
        let cargo = t.write("cargo/bin/rustc", b"");
        assert_eq!(rustc_from(None, Some(t.path().join("cargo").into()), None), Some(cargo));
        assert_eq!(worker_from(Some(fake.clone().into())), Some(fake));
        assert_eq!(worker_from(Some(t.path().join("missing").into())), None);
        assert_eq!(first_file(vec![None, Some(t.path().join("nope"))]), None);
        assert!(rustc().is_some(), "tests need a rustc");
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
