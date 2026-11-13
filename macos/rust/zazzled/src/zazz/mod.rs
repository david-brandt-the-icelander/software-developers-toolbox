// The zazz subcommands: pack, add, test and call sandboxed scripts.
use crate::cmd::{Ctx, Output};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub mod pack;

const HELP: &str = "zazz: sandboxed scripts for agents (JSON replies)
pack DIR [-o FILE]  pack a script directory into a deterministic .zazz (default DIR.zazz beside DIR)";
const COMMANDS: &str = "pack help";

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

// Routes a zazz subcommand.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<Output, String> {
    let Some((sub, rest)) = args.split_first() else { return Ok(Output::plain(HELP.into())) };
    match sub.as_str() {
        "pack" => pack::run(ctx, rest),
        "help" | "--help" | "-h" => Ok(Output::plain(HELP.into())),
        other => Err(format!("unknown zazz command '{other}'; try: {COMMANDS}")),
    }
}

// Returns the canonical directories zazz may write output files into: the caller's directory and temp dirs.
fn writable_roots(cwd: &Path) -> Vec<PathBuf> {
    [cwd.to_path_buf(), std::env::temp_dir(), PathBuf::from("/tmp")]
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect()
}

// Resolves an output path whose parent exists and lies under the caller's directory or a temp dir.
pub fn output_path(cwd: &Path, arg: &Path) -> Result<PathBuf, String> {
    let wanted = cwd.join(arg);
    let name = wanted.file_name().ok_or_else(|| format!("{}: not a file name", arg.display()))?;
    let parent = wanted.parent().unwrap_or(cwd);
    let real = parent.canonicalize().map_err(|e| format!("{}: {e}", parent.display()))?;
    if !writable_roots(cwd).iter().any(|r| real.starts_with(r)) {
        return Err(format!(
            "refusing to write {} outside the working directory or a temp directory",
            arg.display()
        ));
    }
    Ok(real.join(name))
}

// Writes a file atomically through a temporary name in the same directory.
pub fn write_new(path: &Path, data: &[u8]) -> Result<(), String> {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = path.with_file_name(format!(".zazz-tmp-{}-{n}", std::process::id()));
    let result = std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path));
    result.inspect_err(|_| drop(std::fs::remove_file(&tmp))).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{run_cmd, TempDir};

    // Checks help and unknown subcommands.
    #[test]
    fn test_run_help_and_unknown() {
        let t = TempDir::new();
        for args in [vec!["zazz"], vec!["zazz", "help"]] {
            assert!(run_cmd(t.path(), &args).text.starts_with("zazz: sandboxed scripts"));
        }
        let o = run_cmd(t.path(), &["zazz", "nope"]);
        assert_eq!((o.status, o.text.contains("unknown zazz command 'nope'")), (2, true));
    }

    // Checks output paths are confined to the working directory and temp directories.
    #[test]
    fn test_output_path_confinement() {
        let t = TempDir::new();
        assert_eq!(output_path(t.path(), Path::new("a.zazz")).expect("cwd"), t.path().join("a.zazz"));
        assert!(output_path(t.path(), Path::new("/tmp/x.zazz")).is_ok());
        assert!(output_path(t.path(), Path::new("/etc/x.zazz")).unwrap_err().contains("refusing"));
        assert!(output_path(t.path(), Path::new("missing/x.zazz")).is_err());
        assert!(output_path(t.path(), Path::new("/")).unwrap_err().contains("not a file name"));
    }

    // Checks atomic writes land and failures leave no temporary file.
    #[test]
    fn test_write_new() {
        let t = TempDir::new();
        let p = t.path().join("f");
        write_new(&p, b"x").expect("write");
        assert_eq!(std::fs::read(&p).expect("read"), b"x");
        assert!(write_new(&t.path().join("no/f"), b"x").is_err());
        let d = t.path().join("dir");
        std::fs::create_dir(&d).expect("mkdir");
        std::fs::write(d.join("keep"), "x").expect("keep");
        assert!(write_new(&d, b"x").is_err());
        assert_eq!(std::fs::read_dir(t.path()).expect("ls").count(), 2);
    }
}
