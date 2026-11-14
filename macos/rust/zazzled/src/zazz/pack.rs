// zazz pack: packs a script directory into a deterministic .zazz and reports its id.
use crate::cmd::{Ctx, Output};
use std::path::{Path, PathBuf};
use zazz_format::zip::Limits;

// Parses `DIR [-o FILE]`.
fn parse(args: &[String]) -> Result<(String, Option<String>), String> {
    let (mut dir, mut out) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--output" => out = Some(it.next().ok_or("-o needs a file name")?.clone()),
            s if s.starts_with('-') => return Err(format!("zazz pack: unknown option {s}")),
            s if dir.is_none() => dir = Some(s.to_string()),
            s => return Err(format!("zazz pack: unexpected argument {s}")),
        }
    }
    Ok((dir.ok_or("usage: zazz pack DIR [-o FILE]")?, out))
}

// Chooses the default output: DIR.zazz beside the directory.
fn default_output(dir: &Path) -> Result<PathBuf, String> {
    let name = dir.file_name().ok_or_else(|| format!("{}: give -o FILE", dir.display()))?;
    Ok(dir.with_file_name(format!("{}.zazz", name.to_string_lossy())))
}

// Packs DIR and writes the archive, replying with its path, id and size.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<Output, String> {
    let (dir, out) = parse(args)?;
    let dir = ctx.cwd.join(&dir).canonicalize().map_err(|e| format!("{dir}: {e}"))?;
    let (bytes, manifest) = zazz_format::pack::pack_dir(&dir, &Limits::DEFAULT)?;
    let target = match out {
        Some(o) => PathBuf::from(o),
        None => default_output(&dir)?,
    };
    let path = super::output_path(&ctx.cwd, &target)?;
    super::write_new(&path, &bytes)?;
    let reply = serde_json::json!({
        "path": path.to_string_lossy(),
        "hash": zazz_format::script_id(&bytes),
        "name": manifest.name,
        "bytes": bytes.len(),
    });
    Ok(Output::plain(reply.to_string()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::testutil::{run_cmd, TempDir};

    const MANIFEST: &str = r#"{"zazz":1,"name":"demo","kind":"rust","entry":"src/main.rs"}"#;

    // Parses a JSON reply.
    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("json reply")
    }

    // Checks pack writes DIR.zazz by default and to -o, with a stable id.
    #[test]
    fn test_pack_default_and_output() {
        let t = TempDir::new();
        t.write("demo/zazz.json", MANIFEST.as_bytes());
        t.write("demo/src/main.rs", b"fn main() {}\n");
        let a = json(&run_cmd(t.path(), &["zazz", "pack", "demo"]).text);
        assert_eq!(a["name"], "demo");
        assert!(t.path().join("demo.zazz").is_file());
        let b = json(&run_cmd(t.path(), &["zazz", "pack", "demo", "-o", "out.zazz"]).text);
        assert_eq!(a["hash"], b["hash"]);
        assert_eq!(b["bytes"], a["bytes"]);
        assert!(b["path"].as_str().expect("path").ends_with("/out.zazz"));
    }

    // Checks argument and packing errors come back as messages.
    #[test]
    fn test_pack_errors() {
        let t = TempDir::new();
        let err = |args: &[&str]| run_cmd(t.path(), args).text;
        assert!(err(&["zazz", "pack"]).contains("usage: zazz pack"));
        assert!(err(&["zazz", "pack", "-x"]).contains("unknown option -x"));
        assert!(err(&["zazz", "pack", "a", "b"]).contains("unexpected argument b"));
        assert!(err(&["zazz", "pack", "a", "-o"]).contains("-o needs"));
        assert!(err(&["zazz", "pack", "missing"]).contains("missing"));
        t.write("demo/zazz.json", MANIFEST.as_bytes());
        assert!(err(&["zazz", "pack", "demo"]).contains("not in the archive"));
        t.write("demo/src/main.rs", b"");
        assert!(err(&["zazz", "pack", "demo", "-o", "/etc/demo.zazz"]).contains("refusing"));
        assert!(err(&["zazz", "pack", "/"]).contains("error"));
    }
}
