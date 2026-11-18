// Finds the rule and skill files that apply to a path: those in its folder, every parent folder and the home folder.
use std::fs;
use std::path::{Path, PathBuf};

mod purpose;

// Usage text printed for help and for wrong arguments.
const USAGE: &str = "usage: zazzle agent get-rules PATH [--purpose NAME]... [--name-only] [--skill-bodies]\nprints each applying file as `==> rule FILE <==` then its text, and each skill as `==> skill FILE <==` then its description, farthest folder first, closest last (closest wins)\nwith --purpose, files marked for other purposes are dropped and a partly marked file prints `FILE:A-B` and only those lines\n--name-only prints just the `rule FILE` or `skill FILE` lines; --skill-bodies prints whole skill files\n";
// Rule files looked for in each folder, relative to it.
const RULE_FILES: [&str; 4] = ["AGENTS.md", "CLAUDE.md", ".claude/CLAUDE.md", ".codex/AGENTS.md"];
// Folders of extra rule files (every .md inside), relative to a folder.
const RULE_DIRS: [&str; 1] = [".claude/rules"];
// Folders whose child folders each hold a SKILL.md, relative to a folder.
const SKILL_DIRS: [&str; 3] = ["agents/skills", ".agents/skills", ".claude/skills"];

// One file that applies, with whether it is a rule or a skill, which 1-based line ranges of it apply (empty means the whole file) and its text if already read.
#[derive(Debug, PartialEq)]
pub struct Found {
    pub kind: &'static str,
    pub path: PathBuf,
    pub lines: Vec<(usize, usize)>,
    pub text: Option<String>,
}

// Options of the get-rules command.
struct Opts<'a> {
    path: &'a String,
    wanted: Vec<String>,
    name_only: bool,
    skill_bodies: bool,
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
            found.push(Found { kind: "rule", path, lines: Vec::new(), text: None });
        }
    }
    for name in RULE_DIRS {
        for path in children(&dir.join(name)) {
            if path.is_file() && path.extension().is_some_and(|e| e == "md") {
                found.push(Found { kind: "rule", path, lines: Vec::new(), text: None });
            }
        }
    }
    for name in SKILL_DIRS {
        for child in children(&dir.join(name)) {
            let path = child.join("SKILL.md");
            if path.is_file() {
                found.push(Found { kind: "skill", path, lines: Vec::new(), text: None });
            }
        }
    }
    found
}

// Returns the folders to look in, root down to the start folder, with the home folder first when the start is not under it.
fn chain(start: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = start.ancestors().map(Path::to_path_buf).collect();
    dirs.reverse();
    if let Some(home) = home.filter(|h| !start.starts_with(h)) {
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

// Narrows a file to the parts marked for the wanted purposes, or drops it if none are.
fn narrow(mut found: Found, wanted: &[String]) -> Option<Found> {
    let Ok(text) = fs::read_to_string(&found.path) else {
        return Some(found);
    };
    let part = purpose::part(&text, wanted);
    found.text = Some(text);
    match part {
        purpose::Part::Whole => Some(found),
        purpose::Part::Nothing => None,
        purpose::Part::Lines(lines) => {
            found.lines = lines;
            Some(found)
        }
    }
}

// Tells whether a file only imports (`@file` lines) files that are already listed, so printing it would repeat them.
fn only_imports(f: &Found, listed: &[PathBuf]) -> bool {
    let Ok(text) = fs::read_to_string(&f.path) else {
        return false;
    };
    let dir = f.path.parent().unwrap_or(Path::new("/"));
    let mut seen = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some(target) = line.strip_prefix('@') else {
            return false;
        };
        let Ok(full) = fs::canonicalize(dir.join(target)) else {
            return false;
        };
        if full == f.path || !listed.contains(&full) {
            return false;
        }
        seen = true;
    }
    seen
}

// Drops files that repeat another listed file: the same file reached again (a symlink) and files that only import listed files.
fn dedupe(all: Vec<Found>) -> Vec<Found> {
    let canon: Vec<PathBuf> =
        all.iter().map(|f| fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone())).collect();
    let mut kept: Vec<Found> = Vec::new();
    let mut first: Vec<PathBuf> = Vec::new();
    for (f, c) in all.into_iter().zip(&canon) {
        if first.contains(c) || only_imports(&f, &canon) {
            continue;
        }
        first.push(c.clone());
        kept.push(f);
    }
    kept
}

// Lists every rule and skill file that applies to a path, farthest folder first, limited to the wanted purposes when given.
pub fn list_rules(path: &Path, home: Option<&Path>, wanted: &[String]) -> Result<Vec<Found>, String> {
    let start = start_dir(path)?;
    let home = home.and_then(|h| fs::canonicalize(h).ok());
    let all = dedupe(chain(&start, home.as_deref()).iter().flat_map(|d| in_dir(d)).collect());
    if wanted.is_empty() {
        return Ok(all);
    }
    Ok(all.into_iter().filter_map(|f| narrow(f, wanted)).collect())
}

// Splits the arguments after the command into options; a --purpose value must be a non-empty name, not another flag.
fn parse(args: &[String]) -> Option<Opts<'_>> {
    let mut path = None;
    let (mut wanted, mut name_only, mut skill_bodies) = (Vec::new(), false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let value = if a == "--purpose" { Some(it.next()?.as_str()) } else { a.strip_prefix("--purpose=") };
        if let Some(v) = value {
            if v.is_empty() || v.starts_with("--") {
                return None;
            }
            wanted.push(v.to_string());
        } else if a == "--name-only" {
            name_only = true;
        } else if a == "--skill-bodies" {
            skill_bodies = true;
        } else if a.starts_with("--") || path.replace(a).is_some() {
            return None;
        }
    }
    Some(Opts { path: path?, wanted, name_only, skill_bodies })
}

// Formats one found file as one line, or one line per line range.
fn show(f: &Found) -> String {
    if f.lines.is_empty() {
        return format!("{} {}\n", f.kind, f.path.display());
    }
    f.lines.iter().map(|(a, b)| format!("{} {}:{a}-{b}\n", f.kind, f.path.display())).collect()
}

// Returns the `description:` line of a skill's front matter, or nothing.
fn description(text: &str) -> Option<&str> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    lines.take_while(|l| *l != "---").find(|l| l.starts_with("description:"))
}

// Formats one found file (or each of its line ranges) as a `==> kind path <==` header followed by its text; a whole skill gets only its description unless skill bodies are wanted.
fn body(f: &Found, skill_bodies: bool) -> String {
    let read;
    let text = match &f.text {
        Some(t) => t.as_str(),
        None => match fs::read_to_string(&f.path) {
            Ok(t) => {
                read = t;
                read.as_str()
            }
            Err(e) => return format!("==> {} {} <==\nerror: {e}\n\n", f.kind, f.path.display()),
        },
    };
    let head = format!("==> {} {}", f.kind, f.path.display());
    if f.lines.is_empty() && f.kind == "skill" && !skill_bodies {
        return format!("{head} <==\n{}\n\n", description(text).unwrap_or(""));
    }
    let lines: Vec<&str> = text.lines().collect();
    let whole = [(1, lines.len())];
    let ranges = if f.lines.is_empty() { &whole[..] } else { &f.lines[..] };
    let mut out = String::new();
    for &(a, b) in ranges {
        let tag = if f.lines.is_empty() { String::new() } else { format!(":{a}-{b}") };
        out.push_str(&format!("{head}{tag} <==\n"));
        for line in lines
            .iter()
            .take(b)
            .skip(a.saturating_sub(1))
            .filter(|l| f.lines.is_empty() || !purpose::is_marker(l))
        {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

// Runs the command line (after the program name) and returns the text to print and the exit code.
pub fn run(args: &[String], home: Option<&Path>) -> (String, i32) {
    if matches!(args, [c] if c == "--version") {
        return (format!("zazzle-agent {}\n", env!("CARGO_PKG_VERSION")), 0);
    }
    if matches!(args, [c] if c == "help" || c == "--help" || c == "-h") {
        return (USAGE.to_string(), 0);
    }
    let parsed = match args.split_first() {
        Some((cmd, rest)) if cmd == "get-rules" => parse(rest),
        _ => None,
    };
    let Some(o) = parsed else {
        return (USAGE.to_string(), 2);
    };
    match list_rules(Path::new(o.path), home, &o.wanted) {
        Ok(found) if o.name_only => (found.iter().map(show).collect(), 0),
        Ok(found) => (found.iter().map(|f| body(f, o.skill_bodies)).collect(), 0),
        Err(e) => (format!("{e}\n"), 66),
    }
}

#[cfg(test)]
mod tests;
