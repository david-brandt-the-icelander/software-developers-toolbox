// zazz test: builds a script directory or .zazz file and runs its tests without registering anything.
use super::add::read_once;
use super::pipeline::{self, err, Snapshots};
use super::Zazz;
use serde_json::{json, Value};
use std::path::Path;
use zazz_format::manifest::Manifest;
use zazz_format::zip::{self, Limits};

// Parses `DIR|FILE [--update]`.
fn parse(args: &[String]) -> Result<(String, bool), Value> {
    let usage = || err("usage", "usage: zazz test DIR|FILE.zazz [--update]");
    let (mut target, mut update) = (None, false);
    for a in args {
        match a.as_str() {
            "--update" => update = true,
            s if s.starts_with('-') || target.is_some() => return Err(usage()),
            s => target = Some(s.to_string()),
        }
    }
    Ok((target.ok_or_else(usage)?, update))
}

// Runs `zazz test`.
pub fn run(z: &Zazz, cwd: &Path, args: &[String]) -> Result<Value, Value> {
    let (target, update) = parse(args)?;
    let path = cwd.join(&target);
    let (bytes, snaps) = if path.is_dir() {
        let (bytes, _) =
            zazz_format::pack::pack_dir(&path, &Limits::DEFAULT).map_err(|e| err("invalid_manifest", &e))?;
        (bytes, Some(Snapshots { dir: path.join("tests/snapshots"), update }))
    } else if update {
        return Err(err("usage", "--update needs a script directory, not a .zazz file"));
    } else {
        (read_once(&path)?, None)
    };
    let a = zip::read(&bytes, &Limits::DEFAULT).map_err(|e| err("invalid_archive", &e))?;
    let m = Manifest::of(&a).map_err(|e| err("invalid_manifest", &e))?;
    let reg = z.registry().map_err(|e| err("store", &e))?;
    let scratch = reg.scratch("test").map_err(|e| err("store", &e))?;
    let built = pipeline::build_and_test(&reg, &z.worker()?, &a, &m, &scratch, snaps);
    let _ = std::fs::remove_dir_all(&scratch);
    let built = built?;
    Ok(json!({"name": m.name, "build": "ok", "tests": built.tests, "warnings": built.warnings}))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks argument parsing.
    #[test]
    fn test_parse() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse(&v(&["d", "--update"])).expect("ok"), ("d".to_string(), true));
        assert!(parse(&v(&[])).is_err());
        assert!(parse(&v(&["a", "b"])).is_err());
        assert!(parse(&v(&["-x"])).is_err());
    }
}
