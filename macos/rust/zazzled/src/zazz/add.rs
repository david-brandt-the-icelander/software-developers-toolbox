// zazz add: reads a .zazz once, builds and tests it unless already known, and registers it by the hash of its bytes.
use super::pipeline::{self, err};
use super::registry::Registry;
use super::Zazz;
use serde_json::{json, Value};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use zazz_format::manifest::Manifest;
use zazz_format::zip::{self, Limits};

// Reads a file once, refusing symlinks, non-regular files and oversized files.
pub fn read_once(path: &Path) -> Result<Vec<u8>, Value> {
    let io = |e: std::io::Error| err("unreadable", &format!("{}: {e}", path.display()));
    let mut f =
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path).map_err(io)?;
    let md = f.metadata().map_err(io)?;
    if !md.is_file() {
        return Err(err("unreadable", &format!("{}: not a regular file", path.display())));
    }
    if md.len() > Limits::DEFAULT.compressed {
        return Err(err(
            "invalid_archive",
            &format!("{}: {} bytes is over the limit", path.display(), md.len()),
        ));
    }
    let mut bytes = Vec::with_capacity(md.len() as usize);
    f.by_ref().take(Limits::DEFAULT.compressed + 1).read_to_end(&mut bytes).map_err(io)?;
    Ok(bytes)
}

// True when a script is fully registered: metadata and the precompiled module are present.
fn registered(reg: &Registry, hex: &str) -> Option<Value> {
    reg.read_meta(hex).filter(|_| reg.wasm_path(hex, "cwasm").is_file())
}

// The reply for a registered script.
fn reply(meta: &Value, status: &str) -> Value {
    json!({
        "hash": meta["id"],
        "name": meta["name"],
        "status": status,
        "runtime": meta["runtime"],
        "tests": meta["tests"],
        "warnings": meta["warnings"],
        "signer": meta["signer"],
        "trusted": meta["verdict"]["trusted"] == true,
    })
}

// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// Asks a freshly registered script for its OpenAPI document; a script that cannot answer is removed again.
fn fetch_openapi(z: &Zazz, reg: &Registry, hex: &str) -> Result<Value, Value> {
    let msg = json!({"t": "openapi", "mode": "agent"});
    let called = super::call::execute(
        z,
        hex,
        &reg.root.join("tmp"),
        msg,
        super::call::Streams::Capture { stdin: Vec::new() },
    );
    match called {
        Ok(c) if c.result["ok"] == true && c.result["output"].is_object() => Ok(c.result["output"].clone()),
        Ok(c) => {
            reg.remove(hex);
            Err(
                json!({"code": "invalid_openapi", "message": "the script declares \"openapi\" but did not return a document", "error": c.result["error"]}),
            )
        }
        Err(e) => {
            reg.remove(hex);
            Err(e)
        }
    }
}

// Builds, tests and publishes a script whose bytes are already in memory.
fn register(z: &Zazz, reg: &Registry, bytes: &[u8], hex: &str, source: &Path) -> Result<Value, Value> {
    let a = zip::read(bytes, &Limits::DEFAULT).map_err(|e| err("invalid_archive", &e))?;
    let m = Manifest::of(&a).map_err(|e| err("invalid_manifest", &e))?;
    let envelope = super::signing::check_signature(&a)?;
    let verdict = envelope.as_ref().map(|e| super::trust::evaluate(z, &e.chain));
    let scratch = reg.scratch(hex).map_err(|e| err("store", &e))?;
    let built = pipeline::build_and_test(reg, &z.worker()?, &a, &m, &scratch, None);
    let _ = std::fs::remove_dir_all(&scratch);
    let mut built = built?;
    if verdict.as_ref().is_some_and(|v| v["trusted"] != true) {
        built.warnings.push("the signer is not a trusted authority here; the script runs as unsigned".into());
    }
    let manifest: Value = serde_json::from_slice(a.get("zazz.json").expect("checked"))
        .map_err(|e| err("invalid_manifest", &e.to_string()))?;
    let meta = json!({
        "id": format!("b3:{hex}"),
        "name": m.name,
        "version": m.version,
        "description": m.description,
        "kind": manifest["kind"],
        "runtime": "wasm",
        "messages": m.messages,
        "manifest": manifest,
        "tests": built.tests,
        "warnings": built.warnings,
        "openapi": null,
        "signer": verdict.as_ref().map(|v| v["signer"].clone()),
        "signature": envelope.as_ref().map(|e| e.to_json()),
        "verdict": verdict,
        "added_at": now_secs(),
        "source": source.to_string_lossy(),
    });
    reg.write_atomic(&reg.wasm_path(hex, "wasm"), &built.wasm).map_err(|e| err("store", &e))?;
    reg.write_atomic(&reg.wasm_path(hex, "cwasm"), &built.cwasm).map_err(|e| err("store", &e))?;
    reg.write_meta(hex, &meta).map_err(|e| err("store", &e))?;
    if m.messages.iter().any(|x| x == "openapi") {
        let doc = fetch_openapi(z, reg, hex)?;
        let mut meta = meta;
        meta["openapi"] = doc;
        reg.write_meta(hex, &meta).map_err(|e| err("store", &e))?;
        return Ok(reply(&meta, "compiled"));
    }
    Ok(reply(&meta, "compiled"))
}

// Runs `zazz add PATH`.
pub fn run(z: &Zazz, cwd: &Path, args: &[String]) -> Result<Value, Value> {
    let [path] = args else { return Err(err("usage", "usage: zazz add FILE.zazz")) };
    let path = cwd.join(path);
    let bytes = read_once(&path)?;
    let hex = zazz_format::script_id(&bytes)[3..].to_string();
    let reg = z.registry().map_err(|e| err("store", &e))?;
    if let Some(meta) = registered(&reg, &hex) {
        return Ok(reply(&meta, "cached"));
    }
    reg.purge_tmp();
    let _lock = reg.lock(&hex).map_err(|e| err("store", &e))?;
    if let Some(meta) = registered(&reg, &hex) {
        return Ok(reply(&meta, "cached"));
    }
    register(z, &reg, &bytes, &hex, &path)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks read_once refuses symlinks, directories, missing and oversized files.
    #[test]
    fn test_read_once() {
        let t = TempDir::new();
        let f = t.write("a.zazz", b"abc");
        assert_eq!(read_once(&f).expect("read"), b"abc");
        std::os::unix::fs::symlink(&f, t.path().join("link")).expect("link");
        assert_eq!(read_once(&t.path().join("link")).unwrap_err()["code"], "unreadable");
        assert!(read_once(t.path()).unwrap_err()["message"]
            .as_str()
            .expect("msg")
            .contains("not a regular file"));
        assert!(read_once(&t.path().join("missing")).is_err());
        let big = std::fs::File::create(t.path().join("big")).expect("big");
        big.set_len(Limits::DEFAULT.compressed + 1).expect("len");
        assert_eq!(read_once(&t.path().join("big")).unwrap_err()["code"], "invalid_archive");
    }
}
