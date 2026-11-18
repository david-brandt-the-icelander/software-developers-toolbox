// Finds the rule and skill files that apply to a path: those in its folder, every parent folder and the home folder.
use std::fs;
use std::path::{Path, PathBuf};

mod front;
mod glob;
mod imports;
pub mod purpose;

// Usage text printed for help and for wrong arguments.
const USAGE: &str = "usage: zazzle agent get-rules PATH [--purpose NAME]... [--name-only] [--skill-bodies]\nprints each applying file as `==> rule FILE <==` then its text, each file it imports as `==> import FILE <==`, and each skill as `==> skill FILE <==` then its description, farthest folder first, closest last (closest wins)\nwith --purpose, files marked for other purposes are dropped and a partly marked file prints `FILE:A-B` and only those lines\n--name-only prints just the `rule FILE`, `import FILE` or `skill FILE` lines; --skill-bodies prints whole skill files\nzazzle agent llm and zazzle agent prompt run open models locally: see zazzle agent llm --help\n";
// Rule files looked for in each folder, relative to it; a later one in the same folder wins over an earlier one.
const RULE_FILES: [&str; 7] = [
    "AGENTS.md",
    "AGENTS.override.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    ".claude/CLAUDE.md",
    ".codex/AGENTS.md",
    ".codex/AGENTS.override.md",
];
// Folders of extra rule files (every .md inside, at any depth), relative to a folder.
const RULE_DIRS: [&str; 1] = [".claude/rules"];
// Folders whose child folders each hold a SKILL.md, relative to a folder.
const SKILL_DIRS: [&str; 3] = ["agents/skills", ".agents/skills", ".claude/skills"];
// How many folders deep rule folders are searched, so a symlink loop ends.
const RULE_DEPTH: usize = 8;
// How many imports deep `@file` imports are followed, as Claude Code does.
const IMPORT_DEPTH: usize = 5;

// What a found file is: a rule, a file a rule imports, or a skill.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Kind {
    Rule,
    Import,
    Skill,
}

impl Kind {
    // The word printed for the kind.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Rule => "rule",
            Kind::Import => "import",
            Kind::Skill => "skill",
        }
    }
}

// One file that applies, with its kind, which 1-based line ranges of it apply (empty means the whole file) and its text, read once when found (nothing if it is not readable text).
#[derive(Debug, PartialEq)]
pub struct Found {
    pub kind: Kind,
    pub path: PathBuf,
    pub lines: Vec<(usize, usize)>,
    pub text: Option<String>,
}

// The path asked about: its full path (folders above it resolved, its own name kept even if a symlink), the folder it is or is in, and whether it is a folder.
struct Target {
    path: PathBuf,
    dir: PathBuf,
    is_dir: bool,
}

// Options of the get-rules command.
struct Opts<'a> {
    path: &'a String,
    wanted: Vec<String>,
    name_only: bool,
    skill_bodies: bool,
}

// Reads a found file's text once.
fn read(kind: Kind, path: PathBuf) -> Found {
    let text = fs::read_to_string(&path).ok();
    Found { kind, path, lines: Vec::new(), text }
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

// Collects the .md files of a rule folder and its subfolders, in name order, down to a depth.
fn md_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    for path in children(dir) {
        if path.is_dir() && depth > 1 {
            md_files(&path, depth - 1, out);
        } else if path.is_file() && path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

// Tells whether a rule file applies to the target: always, unless its front matter has `paths:` globs, matched against the target relative to the folder holding the rules (or to `/` when the target is elsewhere).
fn in_scope(f: &Found, base: &Path, t: &Target) -> bool {
    let Some(globs) = f.text.as_deref().and_then(|x| front::list(x, "paths")) else {
        return true;
    };
    let rel = t.path.strip_prefix(base).or_else(|_| t.path.strip_prefix("/")).unwrap_or(&t.path);
    let rel = rel.to_string_lossy();
    globs.iter().any(|g| glob::matches(g, &rel, t.is_dir))
}

// Returns the rule and skill files that sit in one folder's known places and apply to the target.
fn in_dir(dir: &Path, t: &Target) -> Vec<Found> {
    let mut found = Vec::new();
    for name in RULE_FILES {
        let path = dir.join(name);
        if path.is_file() {
            found.push(read(Kind::Rule, path));
        }
    }
    for name in RULE_DIRS {
        let mut paths = Vec::new();
        md_files(&dir.join(name), RULE_DEPTH, &mut paths);
        found.extend(paths.into_iter().map(|p| read(Kind::Rule, p)).filter(|f| in_scope(f, dir, t)));
    }
    for name in SKILL_DIRS {
        for child in children(&dir.join(name)) {
            let path = child.join("SKILL.md");
            if path.is_file() {
                found.push(read(Kind::Skill, path));
            }
        }
    }
    found
}

// Returns the folders to look in, from the top folder down to the start, with the home folder first when the start is not under it.
fn chain(start: &Path, home: Option<&Path>, top: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        start.ancestors().take_while(|a| a.starts_with(top)).map(Path::to_path_buf).collect();
    dirs.reverse();
    if let Some(home) = home.filter(|h| !start.starts_with(h)) {
        dirs.insert(0, home.to_path_buf());
    }
    dirs
}

// Resolves the path asked about, failing if it does not exist; its own name is not resolved, so a symlinked file gets the rules of the folder it sits in.
fn target(path: &Path) -> Result<Target, String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let is_dir = fs::metadata(path).map_err(err)?.is_dir();
    let full = match (path.parent(), path.file_name()) {
        (Some(p), Some(name)) => {
            let p = if p.as_os_str().is_empty() { Path::new(".") } else { p };
            fs::canonicalize(p).map_err(err)?.join(name)
        }
        _ => fs::canonicalize(path).map_err(err)?,
    };
    let dir = if is_dir { full.clone() } else { full.parent().unwrap_or(&full).to_path_buf() };
    Ok(Target { path: full, dir, is_dir })
}

// Keeps a file whole when no purposes are wanted, else narrows it to the parts marked for them or drops it if none are.
fn narrow(mut found: Found, wanted: &[String]) -> Option<Found> {
    let Some(text) = found.text.as_deref().filter(|_| !wanted.is_empty()) else {
        return Some(found);
    };
    match purpose::part(text, wanted) {
        purpose::Part::Whole => Some(found),
        purpose::Part::Nothing => None,
        purpose::Part::Lines(lines) => {
            found.lines = lines;
            Some(found)
        }
    }
}

// Tells whether a file only imports (`@file` lines) other files that are already listed, so printing it would repeat them.
fn only_imports(f: &Found, own: &Path, listed: &[PathBuf], home: Option<&Path>) -> bool {
    let Some(text) = &f.text else {
        return false;
    };
    let dir = f.path.parent().unwrap_or(Path::new("/"));
    let mut seen = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some(full) = line.strip_prefix('@').and_then(|t| imports::resolve(t, dir, home)) else {
            return false;
        };
        if full == own || !listed.contains(&full) {
            return false;
        }
        seen = true;
    }
    seen
}

// Drops files that repeat another listed file: the same file reached again (a symlink) and files that only import listed files.
fn dedupe(all: Vec<Found>, home: Option<&Path>) -> Vec<Found> {
    let canon: Vec<PathBuf> =
        all.iter().map(|f| fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone())).collect();
    let mut kept: Vec<Found> = Vec::new();
    let mut first: Vec<PathBuf> = Vec::new();
    for (f, c) in all.into_iter().zip(&canon) {
        if first.contains(c) || only_imports(&f, c, &canon, home) {
            continue;
        }
        first.push(c.clone());
        kept.push(f);
    }
    kept
}

// Returns the lines of a text that apply: all of it, or only the given 1-based ranges.
fn selected(text: &str, lines: &[(usize, usize)]) -> String {
    if lines.is_empty() {
        return text.to_string();
    }
    let all: Vec<&str> = text.lines().collect();
    lines.iter().flat_map(|&(a, b)| all[a - 1..b.min(all.len())].iter()).map(|l| format!("{l}\n")).collect()
}

// Appends the files a rule or import imports, each once and narrowed like the rest, followed by their own imports down to a depth.
fn add_imports(
    f: &Found,
    depth: usize,
    ctx: (Option<&Path>, &[String]),
    listed: &mut Vec<PathBuf>,
    out: &mut Vec<Found>,
) {
    let Some(text) = f.text.as_deref().filter(|_| depth > 0 && f.kind != Kind::Skill) else {
        return;
    };
    let dir = f.path.parent().unwrap_or(Path::new("/"));
    for path in imports::imports(&selected(text, &f.lines), dir, ctx.0) {
        if listed.contains(&path) {
            continue;
        }
        listed.push(path.clone());
        let Some(g) = narrow(read(Kind::Import, path), ctx.1) else { continue };
        let mut sub = Vec::new();
        add_imports(&g, depth - 1, ctx, listed, &mut sub);
        out.push(g);
        out.append(&mut sub);
    }
}

// Lists every rule, imported and skill file that applies to a path, farthest folder first, looking no higher than `top` (`/` outside tests), limited to the wanted purposes when given.
pub fn list_rules(
    path: &Path,
    home: Option<&Path>,
    wanted: &[String],
    top: &Path,
) -> Result<Vec<Found>, String> {
    let t = target(path)?;
    let home = home.and_then(|h| fs::canonicalize(h).ok());
    let home = home.as_deref();
    let all = dedupe(chain(&t.dir, home, top).iter().flat_map(|d| in_dir(d, &t)).collect(), home);
    let kept: Vec<Found> = all.into_iter().filter_map(|f| narrow(f, wanted)).collect();
    let mut listed: Vec<PathBuf> = kept.iter().filter_map(|f| fs::canonicalize(&f.path).ok()).collect();
    let mut out = Vec::new();
    for f in kept {
        let mut sub = Vec::new();
        add_imports(&f, IMPORT_DEPTH, (home, wanted), &mut listed, &mut sub);
        out.push(f);
        out.append(&mut sub);
    }
    Ok(out)
}

// Splits the arguments after the command into options, or says what is wrong with them.
fn parse(args: &[String]) -> Result<Opts<'_>, String> {
    let mut path = None;
    let (mut wanted, mut name_only, mut skill_bodies) = (Vec::new(), false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let value = if a == "--purpose" {
            Some(it.next().map_or("", String::as_str))
        } else {
            a.strip_prefix("--purpose=")
        };
        if let Some(v) = value {
            if !purpose::valid(v) {
                return Err(format!(
                    "--purpose needs a name of letters, digits, `_`, `.` and `-`, starting with a letter or digit and not ending in `-`: `{v}`"
                ));
            }
            wanted.push(v.to_string());
        } else if a == "--name-only" {
            name_only = true;
        } else if a == "--skill-bodies" {
            skill_bodies = true;
        } else if a.starts_with("--") {
            return Err(format!("unknown option {a}"));
        } else if path.replace(a).is_some() {
            return Err("give one PATH".to_string());
        }
    }
    let path = path.ok_or("give a PATH")?;
    Ok(Opts { path, wanted, name_only, skill_bodies })
}

// Tells whether the arguments ask for help: `help`, `--help` or `-h` alone or after get-rules.
fn wants_help(args: &[String]) -> bool {
    let help = |a: &String| a == "help" || a == "--help" || a == "-h";
    match args {
        [first, ..] if help(first) => true,
        [cmd, rest @ ..] if cmd == "get-rules" => rest.iter().any(|a| a == "--help" || a == "-h"),
        _ => false,
    }
}

// Formats one found file as one line, or one line per line range.
pub fn show(f: &Found) -> String {
    let kind = f.kind.name();
    if f.lines.is_empty() {
        return format!("{kind} {}\n", f.path.display());
    }
    f.lines.iter().map(|(a, b)| format!("{kind} {}:{a}-{b}\n", f.path.display())).collect()
}

// Formats one found file (or each of its line ranges) as a `==> kind path <==` header followed by its text; a whole skill gets only its description unless skill bodies are wanted.
pub fn body(f: &Found, skill_bodies: bool) -> String {
    let head = format!("==> {} {}", f.kind.name(), f.path.display());
    let Some(text) = f.text.as_deref() else {
        let e = fs::read_to_string(&f.path).err().map_or("not text".to_string(), |e| e.to_string());
        return format!("{head} <==\nerror: {e}\n\n");
    };
    if f.lines.is_empty() && f.kind == Kind::Skill && !skill_bodies {
        let d = front::field(text, "description").map(|d| format!("description: {d}")).unwrap_or_default();
        return format!("{head} <==\n{d}\n\n");
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

// Runs the command line (after the program name), looking no higher than `top`, and returns the text to print and the exit code.
pub fn run(args: &[String], home: Option<&Path>, top: &Path) -> (String, i32) {
    if matches!(args, [c] if c == "--version") {
        return (format!("zazzle-agent {}\n", env!("CARGO_PKG_VERSION")), 0);
    }
    if wants_help(args) {
        return (USAGE.to_string(), 0);
    }
    let parsed = match args.split_first() {
        Some((cmd, rest)) if cmd == "get-rules" => parse(rest),
        _ => Err("unknown command".to_string()),
    };
    let o = match parsed {
        Ok(o) => o,
        Err(e) => return (format!("{e}\n{USAGE}"), 2),
    };
    match list_rules(Path::new(o.path), home, &o.wanted, top) {
        Ok(found) if o.name_only => (found.iter().map(show).collect(), 0),
        Ok(found) => (found.iter().map(|f| body(f, o.skill_bodies)).collect(), 0),
        Err(e) => (format!("{e}\n"), 66),
    }
}

#[cfg(test)]
mod tests;
