// Finds the rule and skill files that apply to a path: those in its folder, every parent folder and the home folder.
use std::fs;
use std::path::{Path, PathBuf};

mod check;
mod explain;
mod front;
mod glob;
mod imports;
pub mod purpose;

// Usage text printed for help and for wrong arguments.
const USAGE: &str = "usage: zazzle agent get-rules PATH [--purpose NAME]... [--name-only] [--skill-bodies]\n       zazzle agent check [PATH] [--json]\n       zazzle agent explain PATH [--purpose NAME]... [--json]\nget-rules prints each applying file as `==> rule FILE <==` then its text, each file it imports as `==> import FILE <==`, and each skill as `==> skill FILE <==` then its description, farthest folder first, closest last (closest wins)\nwith --purpose, files marked for other purposes are dropped and a partly marked file prints `FILE:A-B` and only those lines\n--name-only prints just the `rule FILE`, `import FILE` or `skill FILE` lines; --skill-bodies prints whole skill files\ncheck prints `FILE:LINE: problem` for each `@file` import that points nowhere, `paths:` glob that matches no file, and skill whose front matter lacks `name` or `description` or names another folder, in the files that apply at PATH (default .) and in folders below it; exit 1 when there are problems\nexplain lists each file that applies to PATH and why (its folder, the `paths:` glob that matched, the file that imported it), and each file found that does not apply and why\n--json prints a JSON array instead; --version prints the version\nzazzle agent llm and zazzle agent prompt run open models locally: see zazzle agent llm --help\n";
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
// The commands the program runs.
const COMMANDS: [&str; 3] = ["get-rules", "check", "explain"];

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

// Why a found file applies to the path (its folder, a matching `paths:` glob in that folder, or the file importing it), or why it does not (no glob matched, other purposes only, a repeat of a listed file, only imports of listed files).
#[derive(Debug, PartialEq, Clone)]
pub enum Why {
    Folder(PathBuf),
    Glob(PathBuf, String),
    Import(PathBuf),
    NoGlob(PathBuf, Vec<String>),
    Purpose,
    Repeat(PathBuf),
    OnlyImports,
}

impl Why {
    // Tells whether the file applies.
    pub fn applies(&self) -> bool {
        matches!(self, Why::Folder(_) | Why::Glob(..) | Why::Import(_))
    }
}

// One file found for a path with why it applies or not.
#[derive(Debug, PartialEq)]
pub struct Traced {
    pub found: Found,
    pub why: Why,
}

// The path asked about: its full path (folders above it resolved, its own name kept even if a symlink), the folder it is or is in, and whether it is a folder.
struct Target {
    path: PathBuf,
    dir: PathBuf,
    is_dir: bool,
}

// Options of a command: its PATH (if given), the wanted purposes and its flags.
struct Opts<'a> {
    path: Option<&'a String>,
    wanted: Vec<String>,
    name_only: bool,
    skill_bodies: bool,
    json: bool,
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

// Decides a rule file's scope: unscoped, the first of its `paths:` globs that matches the target relative to the folder holding the rules (or to `/` when the target is elsewhere), or all its globs when none matches.
fn scope(f: &Found, base: &Path, t: &Target) -> Result<Option<String>, Vec<String>> {
    let Some(globs) = f.text.as_deref().and_then(|x| front::list(x, "paths")) else {
        return Ok(None);
    };
    let rel = t.path.strip_prefix(base).or_else(|_| t.path.strip_prefix("/")).unwrap_or(&t.path);
    let rel = rel.to_string_lossy();
    match globs.iter().find(|g| glob::matches(g, &rel, t.is_dir)) {
        Some(g) => Ok(Some(g.clone())),
        None => Err(globs),
    }
}

// Tells whether a folder has any of the rule or skill places looked for.
fn holds_rules(dir: &Path) -> bool {
    RULE_FILES.iter().chain(&RULE_DIRS).chain(&SKILL_DIRS).any(|n| dir.join(n).exists())
}

// Returns the rule and skill files in one folder's known places, each with why it applies to the target or not.
fn in_dir(dir: &Path, t: &Target) -> Vec<Traced> {
    let here = |found: Found| Traced { found, why: Why::Folder(dir.to_path_buf()) };
    let mut out = Vec::new();
    for name in RULE_FILES {
        let path = dir.join(name);
        if path.is_file() {
            out.push(here(read(Kind::Rule, path)));
        }
    }
    for name in RULE_DIRS {
        let mut paths = Vec::new();
        md_files(&dir.join(name), RULE_DEPTH, &mut paths);
        for found in paths.into_iter().map(|p| read(Kind::Rule, p)) {
            let why = match scope(&found, dir, t) {
                Ok(None) => Why::Folder(dir.to_path_buf()),
                Ok(Some(g)) => Why::Glob(dir.to_path_buf(), g),
                Err(globs) => Why::NoGlob(dir.to_path_buf(), globs),
            };
            out.push(Traced { found, why });
        }
    }
    for name in SKILL_DIRS {
        for child in children(&dir.join(name)) {
            let path = child.join("SKILL.md");
            if path.is_file() {
                out.push(here(read(Kind::Skill, path)));
            }
        }
    }
    out
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

// Keeps a file whole when no purposes are wanted, else narrows it to the parts marked for them; false when none are.
fn narrow(found: &mut Found, wanted: &[String]) -> bool {
    let Some(text) = found.text.as_deref().filter(|_| !wanted.is_empty()) else {
        return true;
    };
    match purpose::part(text, wanted) {
        purpose::Part::Whole => true,
        purpose::Part::Nothing => false,
        purpose::Part::Lines(lines) => {
            found.lines = lines;
            true
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

// Marks applying files that repeat another listed file: the same file reached again (a symlink) and files that only import listed files.
fn dedupe(mut all: Vec<Traced>, home: Option<&Path>) -> Vec<Traced> {
    let canon = |f: &Found| fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone());
    let listed: Vec<PathBuf> = all.iter().filter(|x| x.why.applies()).map(|x| canon(&x.found)).collect();
    let mut first: Vec<(PathBuf, PathBuf)> = Vec::new();
    for x in all.iter_mut().filter(|x| x.why.applies()) {
        let c = canon(&x.found);
        if let Some((_, path)) = first.iter().find(|(f, _)| *f == c) {
            x.why = Why::Repeat(path.clone());
        } else if only_imports(&x.found, &c, &listed, home) {
            x.why = Why::OnlyImports;
        } else {
            first.push((c, x.found.path.clone()));
        }
    }
    all
}

// Returns the lines of a text that apply: all of it, or only the given 1-based ranges.
fn selected(text: &str, lines: &[(usize, usize)]) -> String {
    if lines.is_empty() {
        return text.to_string();
    }
    let all: Vec<&str> = text.lines().collect();
    lines.iter().flat_map(|&(a, b)| all[a - 1..b.min(all.len())].iter()).map(|l| format!("{l}\n")).collect()
}

// Appends the files a rule or import imports, each once and narrowed like the rest (or marked when no wanted part applies), followed by their own imports down to a depth.
fn add_imports(
    f: &Found,
    depth: usize,
    ctx: (Option<&Path>, &[String]),
    listed: &mut Vec<PathBuf>,
    out: &mut Vec<Traced>,
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
        let mut g = read(Kind::Import, path);
        if !narrow(&mut g, ctx.1) {
            out.push(Traced { found: g, why: Why::Purpose });
            continue;
        }
        let mut sub = Vec::new();
        add_imports(&g, depth - 1, ctx, listed, &mut sub);
        out.push(Traced { found: g, why: Why::Import(f.path.clone()) });
        out.append(&mut sub);
    }
}

// Finds every rule, imported and skill file for a path with why it applies or not, farthest folder first, looking no higher than `top`, narrowed to the wanted purposes when given.
fn trace(
    path: &Path,
    home: Option<&Path>,
    wanted: &[String],
    top: &Path,
) -> Result<(Target, Vec<Traced>), String> {
    let t = target(path)?;
    let home = home.and_then(|h| fs::canonicalize(h).ok());
    let home = home.as_deref();
    let mut all = dedupe(chain(&t.dir, home, top).iter().flat_map(|d| in_dir(d, &t)).collect(), home);
    for x in all.iter_mut().filter(|x| x.why.applies()) {
        if !narrow(&mut x.found, wanted) {
            x.why = Why::Purpose;
        }
    }
    let mut listed: Vec<PathBuf> =
        all.iter().filter(|x| x.why.applies()).filter_map(|x| fs::canonicalize(&x.found.path).ok()).collect();
    let mut out = Vec::new();
    for x in all {
        let mut sub = Vec::new();
        if x.why.applies() {
            add_imports(&x.found, IMPORT_DEPTH, (home, wanted), &mut listed, &mut sub);
        }
        out.push(x);
        out.append(&mut sub);
    }
    Ok((t, out))
}

// Lists every rule, imported and skill file that applies to a path, farthest folder first, looking no higher than `top` (`/` outside tests), limited to the wanted purposes when given.
pub fn list_rules(
    path: &Path,
    home: Option<&Path>,
    wanted: &[String],
    top: &Path,
) -> Result<Vec<Found>, String> {
    let (_, all) = trace(path, home, wanted, top)?;
    Ok(all.into_iter().filter(|x| x.why.applies()).map(|x| x.found).collect())
}

// Splits the arguments after a command into options, accepting only the allowed flags, or says what is wrong with them.
fn parse<'a>(args: &'a [String], allowed: &[&str]) -> Result<Opts<'a>, String> {
    let mut o = Opts { path: None, wanted: Vec::new(), name_only: false, skill_bodies: false, json: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let flag = a.split('=').next().unwrap_or(a);
        if a.starts_with("--") && !allowed.contains(&flag) {
            return Err(format!("unknown option {a}"));
        }
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
            o.wanted.push(v.to_string());
        } else if a == "--name-only" {
            o.name_only = true;
        } else if a == "--skill-bodies" {
            o.skill_bodies = true;
        } else if a == "--json" {
            o.json = true;
        } else if a.starts_with("--") {
            return Err(format!("unknown option {a}"));
        } else if o.path.replace(a).is_some() {
            return Err("give one PATH".to_string());
        }
    }
    Ok(o)
}

// Quotes a text as a JSON string.
fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// Tells whether the arguments ask for help: `help`, `--help` or `-h` alone or after a command.
fn wants_help(args: &[String]) -> bool {
    let help = |a: &String| a == "help" || a == "--help" || a == "-h";
    match args {
        [first, ..] if help(first) => true,
        [cmd, rest @ ..] if COMMANDS.contains(&cmd.as_str()) => {
            rest.iter().any(|a| a == "--help" || a == "-h")
        }
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

// Runs get-rules with its options and returns the text to print and the exit code.
fn get_rules(o: &Opts, path: &Path, home: Option<&Path>, top: &Path) -> (String, i32) {
    match list_rules(path, home, &o.wanted, top) {
        Ok(found) if o.name_only => (found.iter().map(show).collect(), 0),
        Ok(found) => (found.iter().map(|f| body(f, o.skill_bodies)).collect(), 0),
        Err(e) => (format!("{e}\n"), 66),
    }
}

// Runs the command line (after the program name), looking no higher than `top`, and returns the text to print and the exit code (1 only when check finds problems).
pub fn run(args: &[String], home: Option<&Path>, top: &Path) -> (String, i32) {
    if matches!(args, [c] if c == "--version") {
        return (format!("ai-agent-rules {}\n", env!("CARGO_PKG_VERSION")), 0);
    }
    if wants_help(args) {
        return (USAGE.to_string(), 0);
    }
    let (cmd, parsed) = match args.split_first() {
        Some((cmd, rest)) if cmd == "get-rules" => {
            ("get-rules", parse(rest, &["--purpose", "--name-only", "--skill-bodies"]))
        }
        Some((cmd, rest)) if cmd == "check" => ("check", parse(rest, &["--json"])),
        Some((cmd, rest)) if cmd == "explain" => ("explain", parse(rest, &["--purpose", "--json"])),
        _ => ("", Err("unknown command".to_string())),
    };
    let o = match parsed {
        Ok(o) if o.path.is_some() || cmd == "check" => o,
        Ok(_) => return (format!("give a PATH\n{USAGE}"), 2),
        Err(e) => return (format!("{e}\n{USAGE}"), 2),
    };
    let path = Path::new(o.path.map_or(".", String::as_str));
    match cmd {
        "get-rules" => get_rules(&o, path, home, top),
        "check" => check::run(path, home, top, o.json),
        _ => explain::run(path, home, &o.wanted, top, o.json),
    }
}

#[cfg(test)]
mod tests;
