// `zazzle git message FILE`: loads a file, runs the editor on the terminal and saves the result in place, atomically.

use crate::buffer::Buffer;
use crate::clipboard::SystemClipboard;
use crate::editor::{Editor, Outcome};
use crate::files::Entry;
use crate::git::git;
use crate::kind::{split, Kind};
use crate::term::Term;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

// Largest file the editor opens.
pub const MAX_BYTES: u64 = 256 * 1024;

// The help text for `zazzle git message`.
pub const HELP: &str = "usage: zazzle git message FILE

Edits a commit message, pull request body, changelog entry or rebase todo list
in a small editor (no modes).

Exit status 0 means saved, 1 means cancelled (git then aborts).
Keys: Ctrl+S save/commit, Esc cancel, F1 or Ctrl+G help.
";

// What editing ended with: the outcome and the files with their final marks.
pub struct Edited {
    pub outcome: Outcome,
    pub files: Vec<Entry>,
}

// Returns git's comment character (core.commentChar), or '#'.
pub fn comment_char() -> char {
    let v = git(&["config", "--get", "core.commentChar"]).unwrap_or_default();
    match v.trim() {
        "" | "auto" => '#',
        s => s.chars().next().unwrap_or('#'),
    }
}

// Reads the file to edit, refusing large or non-text files; a missing file is empty.
pub fn load(path: &Path) -> Result<String, String> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if meta.len() > MAX_BYTES {
        return Err(format!("{}: larger than {} KiB; use another editor", path.display(), MAX_BYTES / 1024));
    }
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    String::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8 text; use another editor", path.display()))
}

// Writes the text to a temporary file beside the target, flushes it and renames it over the target, keeping its permissions.
pub fn save_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("message");
    let tmp = dir.join(format!(".{name}.zazzle-{}", std::process::id()));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    if let Ok(meta) = fs::metadata(path) {
        fs::set_permissions(&tmp, fs::Permissions::from_mode(meta.permissions().mode()))?;
    }
    fs::rename(&tmp, path)
}

// Opens the file in the editor on the terminal, with a Files view when files are given, and saves it if the user saves.
pub fn edit(path: &Path, files: Vec<Entry>) -> Result<Edited, String> {
    let text = load(path)?;
    let kind = Kind::detect(path);
    let comment = if kind.is_git() { comment_char() } else { '#' };
    let (editable, tail) = split(&text, kind, comment);
    let buf = Buffer::new(editable, tail);
    let mut ed = Editor::new(buf, kind, comment, files, Box::new(SystemClipboard::for_this_system()));
    let outcome = {
        let mut term = Term::open().map_err(|e| format!("needs a terminal: {e}"))?;
        term.run(&mut ed).map_err(|e| e.to_string())?
    };
    if outcome == Outcome::Save {
        save_atomic(path, &ed.buf.text()).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(Edited { outcome, files: ed.files })
}

// Picks the file argument, skipping `+LINE` and `--` that some programs pass to editors.
pub fn file_arg(args: &[String]) -> Result<Option<&str>, String> {
    let rest: Vec<&str> =
        args.iter().map(String::as_str).filter(|a| !a.starts_with('+') && *a != "--").collect();
    match rest.as_slice() {
        [] => Err("missing FILE".to_string()),
        ["-h" | "--help" | "help"] => Ok(None),
        [f] => Ok(Some(f)),
        _ => Err("expected one FILE".to_string()),
    }
}

// Runs `zazzle git message ARGS` and returns the exit code: 0 saved, 1 cancelled or failed.
pub fn main(args: &[String]) -> i32 {
    let file = match file_arg(args) {
        Ok(Some(f)) => f,
        Ok(None) => {
            print!("{HELP}\n{}", crate::offer::HOW_TO);
            return 0;
        }
        Err(e) => {
            eprintln!("zazzle git message: {e}\n\n{HELP}");
            return 2;
        }
    };
    match edit(Path::new(file), Vec::new()) {
        Ok(Edited { outcome: Outcome::Save, .. }) => 0,
        Ok(_) => 1,
        Err(e) => {
            eprintln!("zazzle git message: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Makes an empty scratch directory for a test.
    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("zg-msg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    // Checks argument handling.
    #[test]
    fn test_file_arg() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(file_arg(&v(&["+3", "--", "f"])), Ok(Some("f")));
        assert_eq!(file_arg(&v(&["--help"])), Ok(None));
        assert!(file_arg(&v(&[])).is_err());
        assert!(file_arg(&v(&["a", "b"])).is_err());
        assert_eq!(main(&v(&["-h"])), 0);
        assert_eq!(main(&v(&[])), 2);
    }

    // Checks loading: missing, normal, too large and binary files.
    #[test]
    fn test_load() {
        let d = scratch("load");
        assert_eq!(load(&d.join("missing")), Ok(String::new()));
        fs::write(d.join("ok"), "hi\n").unwrap();
        assert_eq!(load(&d.join("ok")), Ok("hi\n".into()));
        fs::write(d.join("big"), vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
        assert!(load(&d.join("big")).is_err_and(|e| e.contains("larger")));
        fs::write(d.join("bin"), [0xff, 0xfe]).unwrap();
        assert!(load(&d.join("bin")).is_err_and(|e| e.contains("UTF-8")));
        assert!(load(&d.join("ok/x")).is_err());
        fs::remove_dir_all(d).unwrap();
    }

    // Checks the atomic save keeps permissions and leaves no temporary file.
    #[test]
    fn test_save_atomic() {
        let d = scratch("save");
        let p = d.join("COMMIT_EDITMSG");
        fs::write(&p, "old").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        save_atomic(&p, "new\n").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "new\n");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        save_atomic(&d.join("fresh"), "x").unwrap();
        assert!(save_atomic(&d.join("no/such/dir"), "x").is_err());
        fs::remove_dir_all(d).unwrap();
    }

    // Checks the comment character default.
    #[test]
    fn test_comment_char() {
        let c = comment_char();
        assert!(!c.is_whitespace());
    }

    // Checks that editing without a terminal fails cleanly, and that a bad file fails before that.
    #[test]
    fn test_edit_errors() {
        let d = scratch("edit");
        fs::write(d.join("bin"), [0xff]).unwrap();
        assert!(edit(&d.join("bin"), Vec::new()).is_err());
        let v = vec![d.join("bin").to_string_lossy().into_owned()];
        assert_eq!(main(&v), 1);
        fs::remove_dir_all(d).unwrap();
    }
}
