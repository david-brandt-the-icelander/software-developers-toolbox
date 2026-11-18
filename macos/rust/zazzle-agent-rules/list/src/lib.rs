// Finds the rule and skill files that apply to a path: those in its folder, every parent folder and the home folder.
use std::fs;
use std::path::{Path, PathBuf};

// Usage text printed for help and for wrong arguments.
const USAGE: &str = "usage: zazzle agent list-rules PATH\nprints `rule PATH` or `skill PATH` per line, farthest folder first, closest last (closest wins)\n";
// Rule files looked for in each folder, relative to it.
const RULE_FILES: [&str; 4] = ["AGENTS.md", "CLAUDE.md", ".claude/CLAUDE.md", ".codex/AGENTS.md"];
// Folders of extra rule files (every .md inside), relative to a folder.
const RULE_DIRS: [&str; 1] = [".claude/rules"];
// Folders whose child folders each hold a SKILL.md, relative to a folder.
const SKILL_DIRS: [&str; 3] = ["agents/skills", ".agents/skills", ".claude/skills"];

// One file that applies, with whether it is a rule or a skill.
#[derive(Debug, PartialEq)]
pub struct Found {
    pub kind: &'static str,
    pub path: PathBuf,
}

// Returns the sorted children of a folder, or nothing if it cannot be read.
fn children(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    paths.sort();
    paths
}

// Returns the rule and skill files that sit directly in one folder's known places.
fn in_dir(dir: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    for name in RULE_FILES {
        let path = dir.join(name);
        if path.is_file() {
            found.push(Found { kind: "rule", path });
        }
    }
    for name in RULE_DIRS {
        for path in children(&dir.join(name)) {
            if path.is_file() && path.extension().is_some_and(|e| e == "md") {
                found.push(Found { kind: "rule", path });
            }
        }
    }
    for name in SKILL_DIRS {
        for child in children(&dir.join(name)) {
            let path = child.join("SKILL.md");
            if path.is_file() {
                found.push(Found { kind: "skill", path });
            }
        }
    }
    found
}

// Returns the folders to look in: the home folder first, then the root down to the start folder.
fn chain(start: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = start.ancestors().map(Path::to_path_buf).collect();
    dirs.reverse();
    if let Some(home) = home {
        dirs.retain(|d| d != home);
        dirs.insert(0, home.to_path_buf());
    }
    dirs
}

// Resolves a path to the folder it is in (or is), failing if it does not exist.
fn start_dir(path: &Path) -> Result<PathBuf, String> {
    let full = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if full.is_dir() {
        return Ok(full);
    }
    full.parent().map(Path::to_path_buf).ok_or_else(|| format!("{}: no folder", path.display()))
}

// Lists every rule and skill file that applies to a path, farthest folder first.
pub fn list_rules(path: &Path, home: Option<&Path>) -> Result<Vec<Found>, String> {
    let start = start_dir(path)?;
    let home = home.and_then(|h| fs::canonicalize(h).ok());
    Ok(chain(&start, home.as_deref()).iter().flat_map(|d| in_dir(d)).collect())
}

// Runs the command line (after the program name) and returns the text to print and the exit code.
pub fn run(args: &[String], home: Option<&Path>) -> (String, i32) {
    if matches!(args, [c] if c == "--version") {
        return (format!("zazzle-agent {}\n", env!("CARGO_PKG_VERSION")), 0);
    }
    let [cmd, path] = args else {
        let help = matches!(args, [c] if c == "help" || c == "--help" || c == "-h");
        return (USAGE.to_string(), if help { 0 } else { 2 });
    };
    if cmd != "list-rules" {
        return (USAGE.to_string(), 2);
    }
    match list_rules(Path::new(path), home) {
        Ok(found) => (found.iter().map(|f| format!("{} {}\n", f.kind, f.path.display())).collect(), 0),
        Err(e) => (format!("{e}\n"), 66),
    }
}

#[cfg(test)]
mod tests;
