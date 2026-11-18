// `zazzled hook`: the PreToolUse hook for Claude Code and Codex that sends raw searches, reads, listings and builds to zazzle, and logs what it let through.
use serde_json::{json, Value};
use std::path::Path;

pub const USAGE: &str =
    "usage: zazzled hook [--client claude-code|codex] [--edits]  (reads the PreToolUse JSON on stdin)";
const READ_LIMIT: u64 = 24 * 1024;
const RANGE_LINES: u64 = 400;
const ALLOW_MARKER: &str = "# allow-raw";
const WRAPPERS: &[&str] = &["sudo", "command", "exec", "nohup", "time", "env", "xargs", "builtin", "timeout"];
const SEARCHERS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack", "zgrep"];
const READERS: &[&str] = &["cat", "head", "tail", "less", "more", "bat", "nl"];
const FIND_ACTIONS: &[&str] = &["-delete", "-exec", "-execdir", "-ok", "-okdir"];
// Build and test runners whose output zazzle run cleans; with the subcommands that build or test when a tool does more.
const BUILDERS: &[(&str, &[&str])] = &[
    ("cargo", &["build", "test", "check", "clippy", "nextest", "bench", "doc"]),
    ("dotnet", &["build", "test", "publish", "pack", "msbuild"]),
    ("go", &["build", "test", "vet"]),
    ("npm", &["test", "t", "run", "run-script", "ci", "install", "i"]),
    ("pnpm", &["test", "t", "run", "build", "install", "i", "lint"]),
    ("yarn", &["test", "run", "build", "install", "lint"]),
    ("bun", &["test", "run", "install"]),
    ("swift", &["build", "test"]),
    ("mix", &["test", "compile"]),
    ("gradle", &[]),
    ("gradlew", &[]),
    ("mvn", &[]),
    ("mvnw", &[]),
    ("make", &[]),
    ("ninja", &[]),
    ("ctest", &[]),
    ("pytest", &[]),
    ("tsc", &[]),
    ("jest", &[]),
    ("vitest", &[]),
    ("xcodebuild", &[]),
    ("msbuild", &[]),
    ("rspec", &[]),
];
// Edit tools of each client: Claude Code's file tools and Codex's apply_patch.
const EDITORS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit", "apply_patch"];

const SEARCH_MSG: &str = "Search with zazzle (MCP tool zazzle_find, or `zazzle find PATTERN [PATH..]`): matches grouped by file, capped and deduplicated. For C# declarations zazzle_find_symbol answers from the index; zazzle_refs lists uses.";
const FIND_MSG: &str = "List files with zazzle (MCP tool zazzle_ls, or `zazzle ls [DIR]`; `zazzle find -l PATTERN` for files containing text).";
const READ_MSG: &str = "Read with zazzle (MCP tool zazzle_read, or `zazzle read FILE[:A-B]`): a terse view with source line numbers; add source=true (`--source`) for the exact code and the sha every edit needs.";
const RUN_MSG: &str = "Run builds and tests through zazzle: put `zazzle run --` in front of the same command (for example `zazzle run -- cargo test`). It runs unchanged in this shell, keeps the exit code, and cleans the output; `zazzle read-again r:ID --full` gives all of it.";
const EDIT_MSG: &str = "Edit with zazzle (MCP tool zazzle_edit, or `zazzle edit`): read the file with source=true first for its sha, then edit with expect=SHA.";
const WRITE_MSG: &str = "Change files with zazzle edit (MCP tool zazzle_edit, or `zazzle edit FILE --old TEXT --new TEXT --expect SHA`; the sha comes from `zazzle read FILE --source`): it checks the file did not change since you read it and shows the result.";
const ESCAPE: &str =
    " If zazzle cannot do what you need, add the comment `# allow-raw` to the command (it is logged).";

// What the hook decided about one tool call, and the name it is logged under.
#[derive(Debug, PartialEq)]
pub enum Decision {
    Allow,
    Deny { program: String, message: String },
    Bypass { program: String, why: &'static str },
}

// The hook's options: which client calls it and whether native edits are refused.
#[derive(Debug, Default, PartialEq)]
pub struct Opts {
    pub client: String,
    pub edits: bool,
}

// Parses `zazzled hook` options.
pub fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { client: "claude-code".into(), edits: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--client" => o.client = it.next().ok_or(USAGE)?.clone(),
            "--edits" => o.edits = true,
            _ => return Err(USAGE.into()),
        }
    }
    Ok(o)
}

// Splits a shell command into segments of unquoted words at |, ||, &&, ;, &, parentheses, backquotes and newlines; a `#` starting a word ends it.
pub fn segments(cmd: &str) -> Vec<Vec<String>> {
    let mut segs = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    let end_word = |cur: &mut Vec<String>, word: &mut String| {
        if !word.is_empty() {
            cur.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' && chars.peek().is_some() => word.push(chars.next().unwrap_or_default()),
            Some(_) => word.push(c),
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' if chars.peek().is_some() => word.push(chars.next().unwrap_or_default()),
                '#' if word.is_empty() => break,
                '|' | ';' | '&' | '\n' | '(' | ')' | '`' => {
                    end_word(&mut cur, &mut word);
                    if !cur.is_empty() {
                        segs.push(std::mem::take(&mut cur));
                    }
                }
                c if c.is_whitespace() => end_word(&mut cur, &mut word),
                c => word.push(c),
            },
        }
    }
    end_word(&mut cur, &mut word);
    if !cur.is_empty() {
        segs.push(cur);
    }
    segs
}

// Returns the last part of a program path.
fn base(p: &str) -> &str {
    Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or(p)
}

// Skips variable assignments and wrappers (with their flags) and returns the program and its arguments.
fn program(tokens: &[String]) -> Option<(&str, &[String])> {
    let mut i = 0;
    let assign = |t: &str| {
        t.split_once('=')
            .is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_'))
    };
    while i < tokens.len() {
        let t = tokens[i].as_str();
        let wrapped =
            i > 0 && WRAPPERS.contains(&base(&tokens[0])) && (t.starts_with('-') || t.parse::<f64>().is_ok());
        if assign(t) || WRAPPERS.contains(&base(t)) || wrapped {
            i += 1;
        } else {
            break;
        }
    }
    tokens.get(i).map(|p| (base(p), &tokens[i + 1..]))
}

// Says whether one pipeline segment searches, lists or reads files, or builds and tests, and what to use instead.
fn classify(tokens: &[String]) -> Option<(String, &'static str)> {
    let (prog, args) = program(tokens)?;
    let files: Vec<&String> = args
        .iter()
        .filter(|a| !a.starts_with('-') && a.parse::<u64>().is_err() && !a.contains('>') && !a.contains('<'))
        .collect();
    let redirected = tokens.iter().any(|t| t.contains('>') || t.contains("<<"));
    let first = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    if prog == "zazzle" || prog == "zazzled" {
        return None;
    }
    if matches!(prog, "bash" | "sh" | "zsh" | "dash") {
        let at = args.iter().position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))?;
        return segments(args.get(at + 1)?).iter().find_map(|s| classify(s));
    }
    if SEARCHERS.contains(&prog) {
        return Some((prog.into(), SEARCH_MSG));
    }
    if prog == "git" && first == Some("grep") {
        return Some(("git-grep".into(), SEARCH_MSG));
    }
    if prog == "find" && !args.iter().any(|a| FIND_ACTIONS.contains(&a.as_str())) {
        return Some((prog.into(), FIND_MSG));
    }
    let follow = prog == "tail" && args.iter().any(|a| matches!(a.as_str(), "-f" | "-F" | "--follow"));
    if READERS.contains(&prog) && !files.is_empty() && !redirected && !follow {
        return Some((prog.into(), READ_MSG));
    }
    let flag = |a: &str, f: char| a.starts_with('-') && !a.starts_with("--") && a.contains(f);
    if (prog == "sed" && args.iter().any(|a| flag(a, 'i') || a.starts_with("--in-place")))
        || (prog == "perl" && args.iter().any(|a| flag(a, 'i')))
    {
        return Some((format!("{prog} -i"), WRITE_MSG));
    }
    if prog == "sed" && args.iter().any(|a| flag(a, 'n')) && files.len() >= 2 {
        return Some((prog.into(), READ_MSG));
    }
    let builds = |subs: &[&str]| subs.is_empty() || first.is_some_and(|f| subs.contains(&f));
    if BUILDERS.iter().any(|(p, subs)| *p == prog && builds(subs)) {
        return Some((format!("{prog}{}", first.map(|f| format!(" {f}")).unwrap_or_default()), RUN_MSG));
    }
    let python = prog.starts_with("python") && args.windows(2).any(|w| w[0] == "-m" && w[1] == "pytest");
    python.then(|| ("pytest".into(), RUN_MSG))
}

// True when the built-in Read would load a large file whole: over READ_LIMIT bytes with no limit of at most RANGE_LINES lines.
fn too_big(path: &str, input: &Value) -> bool {
    let ranged = input["limit"].as_u64().is_some_and(|n| n <= RANGE_LINES);
    !ranged && std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > READ_LIMIT)
}

// True when a command runs Python from a heredoc or -c that writes files.
fn writes_with_python(cmd: &str) -> bool {
    let python = segments(cmd).iter().any(|s| program(s).is_some_and(|(p, _)| p.starts_with("python")));
    let inline = cmd.contains("<<") || cmd.contains(" -c ");
    let writes =
        [".write(", "write_text(", "write_bytes(", "open(p,'w')", "'w')", "\"w\")", "'a')", "os.replace("];
    python && inline && writes.iter().any(|w| cmd.contains(w))
}

// Quotes a word for the shell when it needs it.
fn sh_quote(w: &str) -> String {
    match w.chars().all(|c| c.is_alphanumeric() || "-_./:=@%+,".contains(c)) {
        true => w.to_string(),
        false => format!("'{}'", w.replace('\'', "'\\''")),
    }
}

// Returns the zazzle command that replaces one redirected segment, when it can be spelled out exactly.
fn exact(tokens: &[String], cmd: &str) -> Option<String> {
    let (prog, args) = program(tokens)?;
    let plain: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    let num = |flag: &str| {
        args.windows(2)
            .find(|w| w[0] == flag)
            .and_then(|w| w[1].parse::<u64>().ok())
            .or_else(|| args.iter().find_map(|a| a.strip_prefix('-').and_then(|n| n.parse::<u64>().ok())))
    };
    let file = |p: &[&str]| p.iter().rev().find(|a| a.parse::<u64>().is_err()).map(|f| sh_quote(f));
    match prog {
        "cat" | "bat" | "nl" | "less" | "more" if plain.len() == 1 => {
            Some(format!("zazzle read {}", sh_quote(plain[0])))
        }
        "head" => Some(format!("zazzle read {}:1-{}", file(&plain)?, num("-n").unwrap_or(10))),
        "sed" => {
            let range = plain.iter().find_map(|a| {
                let (x, y) = a.strip_suffix('p')?.split_once(',')?;
                Some(format!("{}-{}", x.parse::<u64>().ok()?, y.parse::<u64>().ok()?))
            })?;
            Some(format!("zazzle read {}:{range}", file(&plain[1..])?))
        }
        "rg" | "grep" | "egrep" | "fgrep" | "ag" | "ack" if !plain.is_empty() => {
            let paths: Vec<String> = plain[1..].iter().map(|p| sh_quote(p)).collect();
            let i = if args
                .iter()
                .any(|a| a == "-i" || (a.starts_with('-') && !a.starts_with("--") && a.contains('i')))
            {
                " -i"
            } else {
                ""
            };
            Some(format!(
                "zazzle find {}{i}{}",
                sh_quote(plain[0]),
                paths.iter().map(|p| format!(" {p}")).collect::<String>()
            ))
        }
        _ if BUILDERS.iter().any(|(p, _)| *p == prog) || prog.starts_with("python") => {
            Some(format!("zazzle run -- {}", cmd.trim()))
        }
        _ => None,
    }
}

// Decides one tool call from its client's tool name and input.
pub fn decide(o: &Opts, tool: &str, input: &Value) -> Decision {
    let deny = |program: &str, message: String| Decision::Deny { program: program.into(), message };
    match tool {
        "Grep" => deny("Grep", format!("{SEARCH_MSG}{ESCAPE}")),
        "Glob" => deny("Glob", format!("{FIND_MSG}{ESCAPE}")),
        "Read" => {
            let path = input["file_path"].as_str().unwrap_or("");
            if path.to_ascii_lowercase().ends_with(".cs") || o.edits || too_big(path, input) {
                deny("Read", format!("Use: zazzle read {}\n{READ_MSG}", sh_quote(path)))
            } else {
                Decision::Allow
            }
        }
        t if EDITORS.contains(&t) => match o.edits {
            true => deny(t, EDIT_MSG.into()),
            false => Decision::Bypass { program: t.into(), why: "native-edit" },
        },
        "Bash" | "shell" | "exec_command" | "local_shell" => {
            let cmd = match &input["command"] {
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                v => v.as_str().unwrap_or("").to_string(),
            };
            if writes_with_python(&cmd) && !cmd.contains(ALLOW_MARKER) {
                return deny("python-write", format!("{WRITE_MSG}{ESCAPE}"));
            }
            let segs = segments(&cmd);
            let Some((seg, (program, message))) = segs.iter().find_map(|s| classify(s).map(|c| (s, c)))
            else {
                return Decision::Allow;
            };
            if cmd.contains(ALLOW_MARKER) {
                return Decision::Bypass { program, why: "allow-raw" };
            }
            let single = segs.len() == 1;
            match exact(seg, &cmd).filter(|_| single || message != RUN_MSG) {
                Some(e) => deny(&program, format!("Use: {e}\n{message}{ESCAPE}")),
                None => deny(&program, format!("{message}{ESCAPE}")),
            }
        }
        _ => Decision::Allow,
    }
}

// Runs the hook on one PreToolUse payload: returns what to print on stdout (nothing to allow) and logs every decision but a plain allow; `reachable` says whether zazzle answers, else denials become logged bypasses.
pub fn run(o: &Opts, payload: &str, reachable: &dyn Fn() -> bool) -> String {
    if std::env::var("ZAZZLE_HOOK").is_ok_and(|v| v == "off") {
        return String::new();
    }
    let Ok(v) = serde_json::from_str::<Value>(payload) else { return String::new() };
    let tool = v["tool_name"].as_str().unwrap_or("");
    let mut decision = decide(o, tool, &v["tool_input"]);
    if let Decision::Deny { program, .. } = &decision {
        if !reachable() {
            decision = Decision::Bypass { program: program.clone(), why: "zazzle-unavailable" };
        }
    }
    let log = |outcome: &str, program: &str| {
        crate::activity::record(json!({
            "src": "hook",
            "client": o.client,
            "session": v["session_id"],
            "cwd": v["cwd"],
            "tool": tool,
            "program": program,
            "outcome": outcome,
        }))
    };
    match decision {
        Decision::Allow => String::new(),
        Decision::Bypass { program, why } => {
            log(why, &program);
            String::new()
        }
        Decision::Deny { program, message } => {
            log("redirected", &program);
            json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": message,
            }})
            .to_string()
        }
    }
}

// True when the zazzle daemon answers, starting it if needed.
pub fn zazzle_reachable() -> bool {
    let Ok(sock) = crate::client::socket_path() else { return false };
    let req = crate::proto::Request { cwd: "/".into(), session: String::new(), args: vec!["version".into()] };
    crate::client::call_autostart(&sock, &req, &crate::client::spawn_daemon).is_ok()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};

    // Decides a shell command for a client.
    fn bash(cmd: &str) -> Decision {
        decide(&Opts::default(), "Bash", &json!({"command": cmd}))
    }

    // True when the command is redirected with the given advice.
    fn denied(cmd: &str, msg: &str) -> bool {
        matches!(bash(cmd), Decision::Deny { message, .. } if message.contains(msg))
    }

    // Returns the exact replacement a redirect names, if any.
    fn use_line(cmd: &str) -> Option<String> {
        match bash(cmd) {
            Decision::Deny { message, .. } => message.strip_prefix("Use: ")?.lines().next().map(String::from),
            _ => None,
        }
    }

    // Checks the built-in Read is refused for a large file read whole, and allowed for a small file or a short range.
    #[test]
    fn test_read_size() {
        let t = TempDir::new();
        let big = t.write("big.txt", &vec![b'x'; 30_000]);
        let small = t.write("small.txt", b"x");
        let read = |p: &std::path::Path, extra: Value| {
            let mut input = json!({"file_path": p});
            if let Some(l) = extra.get("limit") {
                input["limit"] = l.clone();
            }
            decide(&Opts::default(), "Read", &input)
        };
        assert!(
            matches!(read(&big, json!({})), Decision::Deny { message, .. } if message.starts_with("Use: zazzle read "))
        );
        assert!(matches!(read(&big, json!({"limit": 5000})), Decision::Deny { .. }));
        assert_eq!(read(&big, json!({"limit": 100})), Decision::Allow);
        assert_eq!(read(&small, json!({})), Decision::Allow);
        assert_eq!(read(&t.path().join("missing"), json!({})), Decision::Allow);
    }

    // Checks in-place shell writes go to zazzle edit, while reading sed and Python that only reads pass.
    #[test]
    fn test_shell_writes() {
        for c in [
            "sed -i 's/a/b/' f.rs",
            "sed -i.bak -e x f",
            "sed --in-place=.b x f",
            "perl -pi -e 's/a/b/' f",
            "python3 - <<'EOF'\nopen('f','w').write('x')\nEOF",
            "python3 -c \"import pathlib; pathlib.Path('f').write_text('x')\"",
        ] {
            assert!(denied(c, WRITE_MSG), "{c}");
        }
        for c in ["python3 - <<'EOF'\nprint(open('f').read())\nEOF", "perl -e 'print 1'", "echo 'w') | cat"] {
            assert!(!denied(c, WRITE_MSG), "{c}");
        }
        let allowed = "sed -i x f # allow-raw";
        assert!(matches!(bash(allowed), Decision::Bypass { why: "allow-raw", .. }));
        let py = "python3 - <<'EOF' # allow-raw\nopen('f','w')\nEOF";
        assert!(!denied(py, WRITE_MSG));
    }

    // Checks redirects spell out the zazzle command that replaces a read, search or build.
    #[test]
    fn test_exact_replacements() {
        for (cmd, want) in [
            ("cat src/a.rs", "zazzle read src/a.rs"),
            ("head -n 40 src/a.rs", "zazzle read src/a.rs:1-40"),
            ("head -5 a.rs", "zazzle read a.rs:1-5"),
            ("head a.rs", "zazzle read a.rs:1-10"),
            ("sed -n '10,20p' a.rs", "zazzle read a.rs:10-20"),
            ("rg -i 'fn main' src", "zazzle find 'fn main' -i src"),
            ("grep -rn foo .", "zazzle find foo ."),
            ("cargo test -p x", "zazzle run -- cargo test -p x"),
            ("cat 'a b.rs'", "zazzle read 'a b.rs'"),
        ] {
            assert_eq!(use_line(cmd).as_deref(), Some(want), "{cmd}");
        }
        for cmd in ["cat a b", "sed -n /x/p a.rs", "cargo test | tail", "tail -n 5 a.rs", "find . -name x"] {
            assert_eq!(use_line(cmd), None, "{cmd}");
        }
        for (cmd, want) in [
            ("bat a.rs", "zazzle read a.rs"),
            ("less a.rs", "zazzle read a.rs"),
            ("more a.rs", "zazzle read a.rs"),
            ("egrep x a", "zazzle find x a"),
            ("fgrep --fixed-strings x", "zazzle find x"),
            ("ag x", "zazzle find x"),
            ("ack -i x", "zazzle find x -i"),
            ("pytest -q", "zazzle run -- pytest -q"),
            ("python3 -m pytest", "zazzle run -- python3 -m pytest"),
        ] {
            assert_eq!(use_line(cmd).as_deref(), Some(want), "{cmd}");
        }
        for cmd in [
            "rg",
            "grep",
            "egrep",
            "fgrep",
            "ag",
            "ack",
            "bat a b",
            "nl a b",
            "less a b",
            "more a b",
            "sed -n '1,xp' a b",
            "sed -n 'x,2p' a b",
            "head -n 5 3",
        ] {
            assert_eq!(use_line(cmd), None, "{cmd}");
        }
        let dir = decide(&Opts::default(), "Read", &json!({"file_path": std::env::temp_dir()}));
        assert_eq!(dir, Decision::Allow);
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    // Checks shell commands: searches, listings, reads and builds are redirected; other commands, zazzle itself and harmless forms pass.
    #[test]
    fn test_shell_commands() {
        for c in [
            "rg foo src",
            "grep -rn x .",
            "sudo -E grep x f",
            "env A=1 rg x",
            "git --no-pager grep x",
            "echo $(rg x)",
            "ls | grep x",
        ] {
            assert!(denied(c, SEARCH_MSG), "{c}");
        }
        assert!(denied("find . -name '*.rs'", FIND_MSG));
        assert_eq!(bash("find . -name x -delete"), Decision::Allow);
        for c in ["cat src/a.rs", "head -n 40 a.rs", "sed -n '1,40p' a.rs", "nl a.rs"] {
            assert!(denied(c, READ_MSG), "{c}");
        }
        for c in ["cat > a.txt <<EOF", "tail -f log.txt", "cat", "sed -n p"] {
            assert_eq!(bash(c), Decision::Allow, "{c}");
        }
        for c in [
            "cargo test -p x",
            "cd api && dotnet build",
            "npm test",
            "make",
            "timeout 60 pytest -q",
            "python3 -m pytest",
            "RUST_LOG=1 cargo clippy",
        ] {
            assert!(denied(c, RUN_MSG), "{c}");
        }
        for c in [
            "cargo fmt",
            "npm view x",
            "git status",
            "ls -la",
            "zazzle run -- cargo test",
            "zazzled wrap make",
            "python3 x.py",
            "echo '|' # rg x",
            "",
        ] {
            assert_eq!(bash(c), Decision::Allow, "{c}");
        }
        assert_eq!(bash("rg x # allow-raw"), Decision::Bypass { program: "rg".into(), why: "allow-raw" });
        for c in ["a-b=c ls", "env A=1", "python -m pip install x", "a#b"] {
            assert_eq!(bash(c), Decision::Allow, "{c}");
        }
        assert!(denied("sed --quiet -n p a b", READ_MSG));
        assert!(denied("sed -ni p a b", WRITE_MSG));
        assert_eq!(segments("\"a\\"), vec![vec!["a\\"]]);
        assert!(matches!(bash("cargo test"), Decision::Deny { program, .. } if program == "cargo test"));
        let codex = decide(&Opts::default(), "exec_command", &json!({"command": ["bash", "-lc", "rg x"]}));
        assert!(matches!(codex, Decision::Deny { .. }));
        assert!(denied("bash -lc 'cd api && cargo test'", RUN_MSG));
        for c in ["bash -lc", "sh x.sh", "bash --login -c"] {
            assert_eq!(bash(c), Decision::Allow, "{c}");
        }
        assert_eq!(
            segments(r#"a "b c" 'd|e' f\ g "h\"i" x\"#),
            vec![vec!["a", "b c", "d|e", "f g", "h\"i", "x\\"]]
        );
    }

    // Checks Claude Code's own tools and edit tools, with and without --edits.
    #[test]
    fn test_client_tools() {
        let o = Opts::default();
        let strict = Opts { edits: true, ..Opts::default() };
        assert!(matches!(decide(&o, "Grep", &json!({})), Decision::Deny { .. }));
        assert!(matches!(decide(&o, "Glob", &json!({})), Decision::Deny { .. }));
        assert!(matches!(decide(&o, "Read", &json!({"file_path": "/a/B.CS"})), Decision::Deny { .. }));
        assert_eq!(decide(&o, "Read", &json!({"file_path": "/a/b.rs"})), Decision::Allow);
        assert!(matches!(decide(&strict, "Read", &json!({"file_path": "/a/b.rs"})), Decision::Deny { .. }));
        assert_eq!(
            decide(&o, "apply_patch", &json!({})),
            Decision::Bypass { program: "apply_patch".into(), why: "native-edit" }
        );
        assert!(
            matches!(decide(&strict, "Edit", &json!({})), Decision::Deny { message, .. } if message == EDIT_MSG)
        );
        assert_eq!(decide(&o, "mcp__zazzle__zazzle_read", &json!({})), Decision::Allow);
    }

    // Checks the hook's output and log: a denial in the format both clients read, logged bypasses, fallback when zazzle is down, and the off switch.
    #[test]
    fn test_run_and_log() {
        let _g = env_lock();
        let t = TempDir::new();
        let log = t.path().join("activity.log");
        let saved = std::env::var_os("ZAZZLE_ACTIVITY_LOG");
        std::env::set_var("ZAZZLE_ACTIVITY_LOG", &log);
        let o = parse(&["--client".into(), "codex".into()]).expect("opts");
        let p = |cmd: &str| {
            json!({"session_id": "s1", "cwd": "/w", "tool_name": "Bash", "tool_input": {"command": cmd}})
                .to_string()
        };
        let out: Value = serde_json::from_str(&run(&o, &p("rg x"), &|| true)).expect("json");
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .expect("reason")
            .starts_with("Use: zazzle find x\nSearch with zazzle"));
        assert_eq!(run(&o, &p("rg x"), &|| false), "");
        assert_eq!(run(&o, &p("ls"), &|| true), "");
        assert_eq!(run(&o, &p("rg x # allow-raw"), &|| true), "");
        assert_eq!(run(&o, "not json", &|| true), "");
        std::env::set_var("ZAZZLE_HOOK", "off");
        assert_eq!(run(&o, &p("rg x"), &|| true), "");
        std::env::remove_var("ZAZZLE_HOOK");
        let got = crate::activity::read(&log, 0);
        let outcomes: Vec<&str> = got.iter().map(|e| e["outcome"].as_str().unwrap_or("")).collect();
        assert_eq!(outcomes, ["redirected", "zazzle-unavailable", "allow-raw"]);
        assert_eq!(
            (got[0]["client"].as_str(), got[0]["session"].as_str(), got[0]["program"].as_str()),
            (Some("codex"), Some("s1"), Some("rg"))
        );
        assert_eq!(
            parse(&["--edits".into()]).expect("opts"),
            Opts { client: "claude-code".into(), edits: true }
        );
        assert_eq!(parse(&["-x".into()]).unwrap_err(), USAGE);
        assert_eq!(parse(&["--client".into()]).unwrap_err(), USAGE);
        match saved {
            Some(v) => std::env::set_var("ZAZZLE_ACTIVITY_LOG", v),
            None => std::env::remove_var("ZAZZLE_ACTIVITY_LOG"),
        }
    }
}
