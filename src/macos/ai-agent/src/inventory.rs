// What the model store holds: `status` reports models, partial downloads and disk use; `prune` removes all but the default model.
use crate::{catalog, store};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// What models/ holds for one model file name: the installed file's size, a partial download's size, and whether a lock file exists.
#[derive(Default)]
struct Item {
    model: Option<u64>,
    part: Option<u64>,
    lock: bool,
}

// One file prune removes or would remove: its path, its size and why.
struct Gone {
    path: PathBuf,
    bytes: u64,
    why: String,
}

// Reads models/ into items by model file name, in catalogue order then unknown files by name, and returns the bytes of every file there.
fn scan(root: &Path) -> (Vec<(String, Item)>, u64) {
    let (mut items, mut total) = (BTreeMap::<String, Item>::new(), 0);
    let Ok(dir) = std::fs::read_dir(root.join("models")) else { return (Vec::new(), 0) };
    for e in dir.flatten() {
        let (Ok(name), Ok(meta)) = (e.file_name().into_string(), std::fs::metadata(e.path())) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        total += meta.len();
        let dot = name.strip_prefix('.');
        if let Some(f) = dot.and_then(|n| n.strip_suffix(".part")) {
            items.entry(f.to_string()).or_default().part = Some(meta.len());
        } else if let Some(f) = dot.and_then(|n| n.strip_suffix(".lock")) {
            items.entry(f.to_string()).or_default().lock = true;
        } else if dot.is_none() {
            items.entry(name).or_default().model = Some(meta.len());
        }
    }
    let mut items: Vec<_> = items.into_iter().collect();
    items.sort_by_key(|(f, _)| catalog::MODELS.iter().position(|m| m.file == f).unwrap_or(usize::MAX));
    (items, total)
}

// Returns the catalogue entry for a model file name.
fn by_file(file: &str) -> Option<&'static catalog::Model> {
    catalog::MODELS.iter().find(|m| m.file == file)
}

// Returns the model file the defaults name, if the catalogue knows it.
fn default_file(root: &Path) -> Option<&'static str> {
    catalog::find(&store::defaults(root).model).ok().map(|m| m.file)
}

// Checks an installed file against its catalogue size and SHA-256: "unknown" outside the catalogue, "corrupt" on a mismatch, else what was compared ("size", or "sha256" with verify).
fn check(path: &Path, want: Option<(u64, &str)>, bytes: u64, verify: bool) -> Result<&'static str, String> {
    let Some((size, sha)) = want else { return Ok("unknown") };
    if bytes != size {
        return Ok("corrupt");
    }
    if !verify {
        return Ok("size");
    }
    Ok(if store::sha256(path)? == sha { "sha256" } else { "corrupt" })
}

// Builds `status` as JSON: the defaults, each downloaded model with its check, partial downloads, disk use and the problems that make ok false.
pub fn status(root: &Path, verify: bool) -> Result<Value, String> {
    let d = store::defaults(root);
    let dm = catalog::find(&d.model).ok();
    let (items, files) = scan(root);
    let (mut models, mut partials, mut problems, mut have_default) = (vec![], vec![], vec![], false);
    for (file, it) in &items {
        let m = by_file(file);
        let name = m.map_or(file.as_str(), |m| m.name);
        let busy = it.lock && store::busy(&store::lock_path(root, file));
        let is_default = dm.is_some_and(|d| d.file == file);
        if let Some(bytes) = it.model {
            let c = check(&store::model_path(root, file), m.map(|m| (m.bytes, m.sha256)), bytes, verify)?;
            have_default |= is_default && c != "corrupt";
            if c == "corrupt" {
                problems.push(format!(
                    "{name} is corrupt: `zazzle agent llm --remove {name}`, then download it again"
                ));
            }
            models.push(
                json!({"name": name, "file": file, "bytes": bytes, "expected_bytes": m.map(|m| m.bytes),
                "check": c, "default": is_default, "in_use": busy}),
            );
        }
        if let Some(bytes) = it.part {
            partials.push(json!({"name": name, "file": format!(".{file}.part"), "bytes": bytes,
                "expected_bytes": m.map(|m| m.bytes), "downloading": busy}));
        }
    }
    match dm {
        None => problems.insert(0, format!("default model '{}' is not in the catalogue", d.model)),
        Some(m) if !have_default => problems.insert(
            0,
            format!("default model {} is not downloaded: `zazzle agent llm --model {}`", m.name, m.name),
        ),
        Some(_) => {}
    }
    let disk = files + std::fs::metadata(root.join("defaults")).map_or(0, |m| m.len());
    Ok(json!({
        "tool": "ai-agent",
        "version": env!("CARGO_PKG_VERSION"),
        "ok": problems.is_empty(),
        "store": root.display().to_string(),
        "disk_bytes": disk,
        "default": {"model": d.model, "effort": d.effort, "downloaded": have_default},
        "models": models,
        "partials": partials,
        "problems": problems,
    }))
}

// Renders the JSON of `status` as text.
pub fn status_text(v: &Value) -> String {
    let s = |x: &Value| x.as_str().unwrap_or_default().to_string();
    let gb = |x: &Value| catalog::gb(x.as_u64().unwrap_or_default());
    let list = |x: &Value| x.as_array().cloned().unwrap_or_default();
    let d = &v["default"];
    let here = if d["downloaded"] == true { "downloaded" } else { "not downloaded" };
    let mut out = format!("default: {} at effort {} ({here})\n", s(&d["model"]), s(&d["effort"]));
    out.push_str(if list(&v["models"]).is_empty() { "models: none downloaded\n" } else { "models:\n" });
    for m in list(&v["models"]) {
        let mark = if m["default"] == true { '*' } else { ' ' };
        let what = match m["check"].as_str() {
            Some("size") => "ok (size)".to_string(),
            Some("sha256") => "ok (sha256)".to_string(),
            Some("unknown") => "not in the catalogue".to_string(),
            _ if m["bytes"] != m["expected_bytes"] => {
                format!("CORRUPT: expected {}", gb(&m["expected_bytes"]))
            }
            _ => "CORRUPT: SHA-256 differs".to_string(),
        };
        let busy = if m["in_use"] == true { ", in use" } else { "" };
        out.push_str(&format!("{mark} {:<18} {:>8}  {what}{busy}\n", s(&m["name"]), gb(&m["bytes"])));
    }
    for (i, p) in list(&v["partials"]).iter().enumerate() {
        out.push_str(if i == 0 { "partial downloads:\n" } else { "" });
        let state = if p["downloading"] == true { "downloading" } else { "stale" };
        let of = if p["expected_bytes"].is_null() {
            String::new()
        } else {
            format!(" of {}", gb(&p["expected_bytes"]))
        };
        out.push_str(&format!("  {:<18} {:>8}{of}  {state}\n", s(&p["name"]), gb(&p["bytes"])));
    }
    out.push_str(&format!("store: {} ({} on disk)\n", s(&v["store"]), gb(&v["disk_bytes"])));
    for p in list(&v["problems"]) {
        out.push_str(&format!("problem: {}\n", s(&p)));
    }
    out.push_str(&format!("ai-agent {}\n", s(&v["version"])));
    out
}

// Lists what prune removes for one item: a model other than the default, a partial download unless it is the missing default's, and a lock file once nothing of the model stays.
fn plan(root: &Path, file: &str, it: &Item, is_default: bool) -> Vec<Gone> {
    let name = by_file(file).map_or(file, |m| m.name);
    let mut gone = Vec::new();
    if let Some(bytes) = it.model.filter(|_| !is_default) {
        gone.push(Gone { path: store::model_path(root, file), bytes, why: format!("model {name}") });
    }
    if let Some(bytes) = it.part.filter(|_| !(is_default && it.model.is_none())) {
        gone.push(Gone {
            path: store::part_path(root, file),
            bytes,
            why: format!("stale partial download of {name}"),
        });
    }
    if it.lock && !(is_default && (it.model.is_some() || it.part.is_some())) {
        gone.push(Gone {
            path: store::lock_path(root, file),
            bytes: 0,
            why: format!("orphaned lock of {name}"),
        });
    }
    gone
}

// Removes a file without following a symlink; a file already gone is fine.
fn rm(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("removing {}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

// Runs `prune`: lists (or with apply deletes) every model but the default, stale partial downloads and orphaned locks, skipping a model whose lock is held; returns the report and exit status (2 if a removal failed).
pub fn prune(root: &Path, apply: bool) -> (String, u8) {
    let def = default_file(root);
    let (items, _) = scan(root);
    let (mut done, mut skipped, mut failed, mut kept) = (Vec::new(), Vec::new(), Vec::new(), None);
    for (file, it) in &items {
        let name = by_file(file).map_or(file.as_str(), |m| m.name);
        let is_default = def == Some(file.as_str());
        if is_default && (it.model.is_some() || it.part.is_some()) {
            kept = Some(name);
        }
        let gone = plan(root, file, it, is_default);
        if gone.is_empty() {
            continue;
        }
        let held = match if apply { store::try_lock(root, file) } else { Ok(None) } {
            Ok(h) => h,
            Err(e) => {
                failed.push(format!("error: {e}"));
                continue;
            }
        };
        if if apply { held.is_none() } else { it.lock && store::busy(&store::lock_path(root, file)) } {
            skipped.push(format!("skipped: {name} (in use: a download or run holds its lock)"));
            continue;
        }
        for g in gone {
            match if apply { rm(&g.path) } else { Ok(()) } {
                Ok(()) => done.push(g),
                Err(e) => failed.push(format!("error: {e}")),
            }
        }
        if apply && !it.lock {
            _ = rm(&store::lock_path(root, file));
        }
        drop(held);
    }
    let freed: u64 = done.iter().map(|g| g.bytes).sum();
    let mut out = String::new();
    if !done.is_empty() {
        out.push_str(if apply { "removed:\n" } else { "would remove:\n" });
    }
    for g in &done {
        out.push_str(&format!("  {:>8}  {}  ({})\n", catalog::gb(g.bytes), g.path.display(), g.why));
    }
    for line in skipped.iter().chain(&failed) {
        out.push_str(&format!("{line}\n"));
    }
    if let Some(name) = kept {
        out.push_str(&format!("kept: {name} (the default)\n"));
    }
    out.push_str(&match (done.is_empty(), apply) {
        (true, _) => "nothing to prune\n".to_string(),
        (false, true) => format!("freed {}\n", catalog::gb(freed)),
        (false, false) => {
            format!("would free {}: run `zazzle ai-agent prune --apply` to delete\n", catalog::gb(freed))
        }
    });
    (out, if failed.is_empty() { 0 } else { 2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    // Makes an empty store directory for one test.
    fn store_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ai-agent-inv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("models")).expect("tmp");
        p
    }

    // Writes a file of `len` bytes under models/ without filling it (sparse), so a catalogue-sized model costs no disk.
    fn put(root: &Path, name: &str, len: u64) {
        std::fs::File::create(root.join("models").join(name)).and_then(|f| f.set_len(len)).expect("put");
    }

    // Returns a catalogue entry by name.
    fn model(name: &str) -> &'static catalog::Model {
        catalog::find(name).expect("model")
    }

    // Checks size and SHA-256 checks: unknown outside the catalogue, corrupt on a wrong size or hash, size or sha256 otherwise.
    #[test]
    fn test_inventory_check() {
        let root = store_dir("check");
        let path = root.join("models/x");
        std::fs::write(&path, b"abc").expect("write");
        let sha: String = Sha256::digest(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(check(&path, None, 3, true), Ok("unknown"));
        assert_eq!(check(&path, Some((4, &sha)), 3, true), Ok("corrupt"));
        assert_eq!(check(&path, Some((3, "00")), 3, false), Ok("size"));
        assert_eq!(check(&path, Some((3, &sha)), 3, true), Ok("sha256"));
        assert_eq!(check(&path, Some((3, "00")), 3, true), Ok("corrupt"));
        assert!(check(&root.join("models/gone"), Some((3, "00")), 3, true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    // Checks status on an empty store, with an unknown default, and on a store with every kind of file, as JSON and text.
    #[test]
    fn test_inventory_status() {
        let root = store_dir("status");
        let v = status(&root, false).expect("empty");
        assert_eq!((v["tool"].as_str(), v["ok"].as_bool()), (Some("ai-agent"), Some(false)));
        assert!(v["problems"][0].as_str().unwrap_or_default().contains("qwen3.5-4b is not downloaded"));
        assert!(status_text(&v).contains("models: none downloaded"));
        store::save_defaults(&root, &store::Defaults { model: "nope".into(), effort: "low".into() })
            .expect("save");
        assert!(status(&root, false).expect("unknown")["problems"][0]
            .as_str()
            .unwrap_or_default()
            .contains("not in the catalogue"));
        std::fs::remove_file(root.join("defaults")).expect("rm");

        let (def, small, big) = (model(catalog::DEFAULT), model("qwen3.5-2b"), model("qwen3.5-9b"));
        put(&root, def.file, def.bytes);
        put(&root, small.file, 5);
        put(&root, "old.gguf", 3);
        put(&root, &format!(".{}.part", big.file), 7);
        let v = status(&root, false).expect("status");
        assert_eq!((v["ok"].as_bool(), v["default"]["downloaded"].as_bool()), (Some(false), Some(true)));
        assert_eq!(v["disk_bytes"].as_u64(), Some(def.bytes + 15));
        let checks: Vec<_> =
            v["models"].as_array().expect("models").iter().map(|m| m["check"].clone()).collect();
        assert_eq!(checks, [json!("corrupt"), json!("size"), json!("unknown")]);
        assert_eq!(v["partials"][0]["downloading"], false);
        let text = status_text(&v);
        assert!(text.contains(&format!("* {:<18}   2.7 GB  ok (size)", def.name)), "{text}");
        assert!(text.contains("CORRUPT: expected 1.3 GB") && text.contains("not in the catalogue"), "{text}");
        assert!(
            text.contains("partial downloads:\n  qwen3.5-9b") && text.contains("of 5.7 GB  stale"),
            "{text}"
        );
        assert!(
            text.contains("problem: qwen3.5-2b is corrupt") && text.ends_with("ai-agent 0.0.0-dev\n"),
            "{text}"
        );

        std::fs::remove_file(store::model_path(&root, small.file)).expect("rm");
        let _run = store::try_lock(&root, big.file).expect("lock");
        let v = status(&root, false).expect("healthy");
        assert_eq!((v["ok"].as_bool(), v["partials"][0]["downloading"].as_bool()), (Some(true), Some(true)));
        let _ = std::fs::remove_dir_all(&root);
    }

    // Checks prune lists without deleting, deletes with apply, keeps the default and its partial download, and skips a model whose lock is held.
    #[test]
    fn test_inventory_prune() {
        let root = store_dir("prune");
        let (def, small, big, busy) =
            (model(catalog::DEFAULT), model("qwen3.5-2b"), model("qwen3.5-9b"), model("gemma-4-12b"));
        put(&root, def.file, 4);
        put(&root, &format!(".{}.lock", def.file), 0);
        put(&root, small.file, 5);
        put(&root, "old.gguf", 3);
        put(&root, &format!(".{}.part", big.file), 7);
        put(&root, ".gpt-oss-20b-MXFP4.gguf.lock", 0);
        put(&root, busy.file, 2);
        let held = store::try_lock(&root, busy.file).expect("lock");
        assert_eq!(prune(&store_dir("prune-empty"), false), ("nothing to prune\n".into(), 0));

        let (text, code) = prune(&root, false);
        assert_eq!(code, 0);
        for want in [
            "would remove:\n",
            "(model qwen3.5-2b)",
            "(model old.gguf)",
            "(stale partial download of qwen3.5-9b)",
            "(orphaned lock of gpt-oss-20b)",
            "skipped: gemma-4-12b (in use",
            "kept: qwen3.5-4b (the default)",
            "would free 0.0 GB: run `zazzle ai-agent prune --apply`",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        assert!(
            !text.contains("lock of qwen3.5-4b") && store::model_path(&root, small.file).exists(),
            "{text}"
        );

        let (text, code) = prune(&root, true);
        assert!(code == 0 && text.starts_with("removed:\n") && text.contains("freed 0.0 GB"), "{text}");
        let mut left: Vec<_> = std::fs::read_dir(root.join("models"))
            .expect("dir")
            .map(|e| e.expect("entry").file_name().into_string().expect("utf8"))
            .collect();
        left.sort();
        let want = [
            busy.file.to_string(),
            def.file.to_string(),
            format!(".{}.lock", busy.file),
            format!(".{}.lock", def.file),
        ];
        let mut want = want.to_vec();
        want.sort();
        assert_eq!(left, want);

        drop(held);
        let (text, _) = prune(&root, true);
        assert!(
            text.contains("(model gemma-4-12b)") && text.contains("(orphaned lock of gemma-4-12b)"),
            "{text}"
        );
        assert!(!store::model_path(&root, busy.file).exists() && store::model_path(&root, def.file).exists());

        std::fs::remove_file(store::model_path(&root, def.file)).expect("rm");
        put(&root, &format!(".{}.part", def.file), 9);
        let (text, _) = prune(&root, true);
        assert!(text.contains("kept: qwen3.5-4b") && text.contains("nothing to prune"), "{text}");
        assert!(store::part_path(&root, def.file).exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(store_dir("prune-empty"));
    }
}
