// JIT tools: programs zazzle runs that are not in the release tarball (ai-agent), downloaded on first use from beside the daemon's release and accepted only when their SHA-256 matches the manifest compiled into daemon.
use crate::modules::{self, Store};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/tools.manifest"));
const BASE_URL: &str = "https://icelander.net/downloads/zazzle";

// The tool store: the embedded manifest, ZAZZLE_TOOLS_DIR or ~/.zazzle/v1/tools, and ZAZZLE_TOOLS_URL or this release's tools directory.
pub fn store() -> Option<Store> {
    let dir = match std::env::var_os("ZAZZLE_TOOLS_DIR") {
        Some(d) => PathBuf::from(d),
        None => crate::store::default_dir().ok()?.join("tools"),
    };
    let base = std::env::var("ZAZZLE_TOOLS_URL")
        .unwrap_or_else(|_| format!("{BASE_URL}/v{}/tools/{}", crate::cmd::VERSION, modules::platform()));
    Some(Store { entries: modules::parse(EMBEDDED), dir, base })
}

// Returns a tool's executable beside `exe_dir` when one is there (a development build or a hand install).
fn beside(exe_dir: Option<&Path>, name: &str) -> Option<PathBuf> {
    let p = exe_dir?.join(name);
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).then_some(p)
}

// Returns a tool's executable: beside the daemon, else from the store, downloading it first (with a note on `note`) when it is missing.
pub fn path(
    name: &str,
    exe_dir: Option<&Path>,
    store: Option<Store>,
    note: &mut dyn FnMut(&str),
) -> Result<PathBuf, String> {
    if let Some(p) = beside(exe_dir, name) {
        return Ok(p);
    }
    let missing = || {
        format!("{name} is not part of this build: `cargo build --release -p {name}` in src/macos and put it beside the daemon")
    };
    let store = store.ok_or_else(missing)?;
    let e = store.entry(name).ok_or_else(missing)?.clone();
    if !store.installed(&e) {
        note(&format!("fetching {name} for this zazzle (once)"));
        store.fetch(&e)?;
    }
    let p = store.dir.join(&e.file);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
        .map_err(|err| format!("making {} executable: {err}", p.display()))?;
    Ok(p)
}

// Replaces this process with a tool, fetching it first when needed; returns only on failure.
pub fn exec(name: &str, args: &[String]) -> String {
    let exe = std::env::current_exe().ok();
    let dir = exe.as_deref().and_then(Path::parent);
    match path(name, dir, store(), &mut |m| eprintln!("{m}")) {
        Ok(p) => format!("running {}: {}", p.display(), std::process::Command::new(&p).args(args).exec()),
        Err(e) => e,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::modules::Entry;
    use crate::testutil::TempDir;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    // Serves the same body to every request on a local port and returns the base URL.
    fn serve(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head =
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(head.as_bytes()).and_then(|()| s.write_all(&body));
            }
        });
        format!("http://{addr}/tools/test")
    }

    // A store in a temporary directory naming one tool with the SHA-256 of `body`.
    fn store_for(t: &TempDir, body: &[u8], base: &str) -> Store {
        let sha256 = Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
        let entries = vec![Entry { name: "ai-agent".into(), sha256, file: "ai-agent".into() }];
        Store { entries, dir: t.path().join("tools"), base: base.into() }
    }

    // Checks a tool beside the daemon wins, a missing one downloads once and is made executable, and a wrong hash or no entry fails.
    #[test]
    fn test_tools_path() {
        let t = TempDir::new();
        let body = b"#!/bin/sh\necho hi\n".to_vec();
        let mut notes = Vec::new();
        let p = path("ai-agent", None, Some(store_for(&t, &body, &serve(body.clone()))), &mut |m| {
            notes.push(m.to_string())
        })
        .expect("fetch");
        assert_eq!((std::fs::read(&p).expect("read"), notes.len()), (body.clone(), 1));
        assert_eq!(std::fs::metadata(&p).expect("meta").permissions().mode() & 0o777, 0o755);
        let again = path("ai-agent", None, Some(store_for(&t, &body, "http://127.0.0.1:9")), &mut |_| {
            panic!("no second fetch")
        });
        assert_eq!(again, Ok(p.clone()));

        let bin = t.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::copy(&p, bin.join("ai-agent")).expect("copy");
        assert_eq!(path("ai-agent", Some(&bin), None, &mut |_| {}), Ok(bin.join("ai-agent")));

        let t2 = TempDir::new();
        let wrong = store_for(&t2, b"other", &serve(body));
        assert!(path("ai-agent", None, Some(wrong), &mut |_| {})
            .expect_err("hash")
            .contains("does not match"));
        let none = Store { entries: Vec::new(), dir: t2.path().into(), base: String::new() };
        assert!(path("ai-agent", None, Some(none), &mut |_| {})
            .expect_err("entry")
            .contains("not part of this build"));
        assert!(path("ai-agent", None, None, &mut |_| {}).is_err());
    }

    // Checks the store reads its directory and URL from the environment and defaults to this release's tools directory.
    #[test]
    fn test_tools_store_env() {
        let _g = crate::testutil::env_lock();
        let t = TempDir::new();
        std::env::set_var("ZAZZLE_TOOLS_DIR", t.path());
        std::env::set_var("ZAZZLE_TOOLS_URL", "http://127.0.0.1:9");
        let s = store().expect("store");
        assert_eq!((s.dir.as_path(), s.base.as_str()), (t.path(), "http://127.0.0.1:9"));
        std::env::remove_var("ZAZZLE_TOOLS_DIR");
        std::env::remove_var("ZAZZLE_TOOLS_URL");
        assert!(store().expect("default").base.ends_with(&format!("/tools/{}", modules::platform())));
        assert!(EMBEDDED.is_empty() || !modules::parse(EMBEDDED).is_empty());
    }
}
