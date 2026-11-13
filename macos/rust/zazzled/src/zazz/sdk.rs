// The zazz SDK sources, embedded in zazzled and compiled once per toolchain and target into an rlib for scripts.
use super::proc;
use super::tools;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const BUILD_TIMEOUT: Duration = Duration::from_secs(300);

// Expands to the list of SDK source files, each with its text.
macro_rules! sdk_files {
    ($($name:literal),* $(,)?) => { &[$(($name, include_str!(concat!("../../../zazz-sdk/src/", $name)))),*] };
}

pub const FILES: &[(&str, &str)] = sdk_files!(
    "lib.rs",
    "call.rs",
    "cliargs.rs",
    "error.rs",
    "fdpass.rs",
    "handlers.rs",
    "http.rs",
    "json.rs",
    "native.rs",
    "proto.rs",
    "render.rs",
    "wasm.rs",
    "test/mod.rs",
    "test/check.rs",
    "test/fakes.rs",
);

// A hash of every SDK source, part of every build key.
pub fn hash() -> String {
    let mut h = blake3::Hasher::new();
    for (name, text) in FILES {
        h.update(name.as_bytes());
        h.update(&[0]);
        h.update(text.as_bytes());
        h.update(&[0]);
    }
    h.finalize().to_hex()[..16].to_string()
}

// The build key for a toolchain and target: changes whenever either, or the SDK, changes.
pub fn key(rustc_version: &str, target: &str) -> String {
    let k = blake3::hash(format!("{rustc_version}\n{target}\n{}", hash()).as_bytes());
    k.to_hex()[..16].to_string()
}

// Writes the SDK sources into a directory.
fn write_sources(dir: &Path) -> Result<(), String> {
    for (name, text) in FILES {
        let p = dir.join("src").join(name);
        std::fs::create_dir_all(p.parent().expect("SDK files live under src/")).map_err(|e| e.to_string())?;
        std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

// Returns the directory holding libzazz.rlib for a target, building it first if needed.
pub fn ensure_rlib(root: &Path, rustc: &Path, key: &str, target: &str) -> Result<PathBuf, String> {
    let out = root.join("sdk").join(key);
    if out.join("libzazz.rlib").is_file() {
        return Ok(out);
    }
    let staging = root.join("tmp").join(format!("sdk-{key}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    write_sources(&staging)?;
    let mut cmd = Command::new(rustc);
    tools::rustc_env(&mut cmd).current_dir(&staging).args([
        "--edition",
        "2021",
        "--crate-type",
        "rlib",
        "--crate-name",
        "zazz",
        "-C",
        "opt-level=2",
        "--cap-lints",
        "allow",
        "--target",
        target,
        "src/lib.rs",
        "--out-dir",
        "out",
    ]);
    let r = proc::run(&mut cmd, None, BUILD_TIMEOUT)?;
    if !r.ok() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("building the zazz SDK for {target} failed: {}", r.err_text()));
    }
    std::fs::create_dir_all(out.parent().expect("sdk dir has a parent")).map_err(|e| e.to_string())?;
    let moved = std::fs::rename(staging.join("out"), &out);
    let _ = std::fs::remove_dir_all(&staging);
    match moved {
        Ok(()) => Ok(out),
        Err(_) if out.join("libzazz.rlib").is_file() => Ok(out),
        Err(e) => Err(format!("{}: {e}", out.display())),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks the embedded list covers every SDK source file on disk.
    #[test]
    fn test_files_match_the_sdk_tree() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../zazz-sdk/src");
        let mut on_disk = Vec::new();
        for dir in [src.clone(), src.join("test")] {
            for e in std::fs::read_dir(&dir).expect("sdk dir") {
                let p = e.expect("entry").path();
                if p.extension().is_some_and(|x| x == "rs") {
                    on_disk.push(p.strip_prefix(&src).expect("under src").to_string_lossy().into_owned());
                }
            }
        }
        on_disk.sort();
        let mut listed: Vec<String> = FILES.iter().map(|(n, _)| n.to_string()).collect();
        listed.sort();
        assert_eq!(listed, on_disk);
    }

    // Checks keys depend on toolchain and target.
    #[test]
    fn test_keys() {
        assert_eq!(hash().len(), 16);
        assert_ne!(key("a", "t"), key("b", "t"));
        assert_ne!(key("a", "t"), key("a", "u"));
    }

    // Checks the rlib is built once for WASM and reused, and a broken rustc is reported.
    #[test]
    fn test_ensure_rlib() {
        let t = TempDir::new();
        let rc = tools::rustc().expect("rustc");
        let dir = ensure_rlib(t.path(), &rc, "k1", tools::WASM_TARGET).expect("build");
        assert!(dir.join("libzazz.rlib").is_file());
        assert_eq!(ensure_rlib(t.path(), &rc, "k1", tools::WASM_TARGET).expect("cached"), dir);
        assert!(ensure_rlib(t.path(), &rc, "k2", "no-such-target")
            .unwrap_err()
            .contains("building the zazz SDK"));
        assert!(ensure_rlib(t.path(), Path::new("/nonexistent/rustc"), "k3", tools::WASM_TARGET).is_err());
    }
}
