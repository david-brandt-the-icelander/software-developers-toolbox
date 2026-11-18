// Keeps a long-lived MCP connection on the installed version: when the zazzled program it was started as is replaced by an upgrade, the server re-executes the new program on the same connection, which then tells the client its tool list changed.
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::SystemTime;

// The environment variables that carry the request being answered and the connection's state across the re-execution.
pub const PENDING: &str = "ZAZZLE_MCP_PENDING";
pub const ROOTS: &str = "ZAZZLE_MCP_ROOTS";

// What the started program looked like: the path it was started as, the file it resolved to, and that file's time and size.
#[derive(PartialEq, Debug, Clone)]
pub struct Stamp {
    path: PathBuf,
    target: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
}

static STARTED: OnceLock<Stamp> = OnceLock::new();

// Reads the stamp of the program a path names now.
pub fn stamp(path: &std::path::Path) -> Option<Stamp> {
    let target = std::fs::canonicalize(path).ok()?;
    let meta = std::fs::metadata(&target).ok()?;
    Some(Stamp { path: path.to_path_buf(), target, modified: meta.modified().ok(), len: meta.len() })
}

// Remembers the program this MCP server was started as, so a later upgrade can be noticed.
pub fn remember() {
    let path = std::env::args_os().next().map(PathBuf::from).filter(|p| p.components().count() > 1);
    let found = path.or_else(|| std::env::current_exe().ok()).and_then(|p| stamp(&p));
    if let Some(s) = found {
        let _ = STARTED.set(s);
    }
}

// True when the program at the started path is no longer the one that was started.
pub fn changed(started: &Stamp) -> bool {
    stamp(&started.path).is_some_and(|now| now != *started)
}

// Re-executes the upgraded program on this connection with the pending request and state, when the program changed; returns only when it did not or the exec failed.
pub fn reexec_if_changed(pending: &str, offers_roots: bool, session: &str) {
    let Some(started) = STARTED.get().filter(|s| changed(s)) else { return };
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&started.path)
        .args(std::env::args_os().skip(1))
        .env(PENDING, pending)
        .env(ROOTS, if offers_roots { "1" } else { "0" })
        .env("ZAZZLE_SESSION", session)
        .exec();
    eprintln!("zazzled mcp: could not start the upgraded program: {err}");
}

// Takes the request and state a previous program handed over, once.
pub fn resumed() -> Option<(String, bool)> {
    let line = std::env::var(PENDING).ok().filter(|l| !l.is_empty())?;
    let roots = std::env::var(ROOTS).is_ok_and(|v| v == "1");
    std::env::remove_var(PENDING);
    std::env::remove_var(ROOTS);
    Some((line, roots))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};

    // Checks a replaced program is noticed, an unchanged one is not, and a missing one is not taken for an upgrade.
    #[test]
    fn test_changed() {
        let t = TempDir::new();
        let p = t.write("zazzled", b"old");
        let s = stamp(&p).expect("stamp");
        assert!(!changed(&s));
        t.write("zazzled", b"newer program");
        assert!(changed(&s));
        std::fs::remove_file(&p).expect("rm");
        assert!(!changed(&s));
        assert!(stamp(&t.path().join("none")).is_none());
        reexec_if_changed("{}", false, "s");
    }

    // Checks the handed-over request is read once.
    #[test]
    fn test_resumed() {
        let _g = env_lock();
        std::env::set_var(PENDING, "{\"id\":1}");
        std::env::set_var(ROOTS, "1");
        assert_eq!(resumed(), Some(("{\"id\":1}".to_string(), true)));
        assert_eq!(resumed(), None);
    }
}
