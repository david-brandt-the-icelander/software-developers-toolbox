// Help for people at a terminal: the command overview and one page per command, wrapped to 80 columns.
use super::VERSION;

const WIDTH: usize = 80;
const LEFT: usize = 26;
const GROUPS: [&str; 5] = [
    "Read, search and edit files",
    "Repository structure (from zazzle-csharp-indexer's index)",
    "Command output",
    "Setup and the daemon",
    "Other tools",
];
// The commands agents see, the one list behind agent help, the MCP tool list and the agent instructions; every other command is for people and shows only in `help --all`.
pub const AGENT: &[&str] =
    &["read", "outline", "find", "refs", "find-symbol", "edit", "ls", "run", "clean", "read-again"];

// True when agents are shown the command.
pub fn is_agent(cmd: &str) -> bool {
    AGENT.contains(&cmd)
}

const COMMON: &[(&str, &str)] = &[
    ("--budget T", "Cap the output at about T tokens (default 3000; read, read-again and clean have their own)"),
    ("--disable-session-deduping", "Send output again even if this session has already seen it"),
    (
        "--commit SHA",
        "Repository commands only: answer only from checkouts at this commit (a prefix, or HEAD), and say where it is checked out otherwise",
    ),
];

// One command's help: where the overview lists it, how it is called and what its options do.
struct Page {
    name: &'static str,
    group: usize,
    brief: &'static str,
    summary: &'static str,
    usage: &'static [&'static str],
    about: &'static [&'static str],
    options: &'static [(&'static str, &'static str)],
    examples: &'static [&'static str],
}

const PAGES: &[Page] = &[
    Page {
        name: "find",
        group: 0,
        brief: "find PATTERN [PATH...]",
        summary: "Search files for a regular expression",
        usage: &["zazzle find PATTERN [PATH...] [OPTIONS]"],
        about: &[
            "Searches PATH (default: the current folder), skipping what .gitignore ignores. Matching lines start with N: and context lines with N-. Ordinary files come before deeply nested files and tests, long lines are clipped around the match, and a footer says how to narrow a search that hit the limit.",
            "PATTERN is a Rust regular expression: no look-around and no backreferences.",
        ],
        options: &[
            ("-i, --ignore-case", "Ignore case"),
            ("-F, --fixed", "Match PATTERN as plain text"),
            ("-w, --word", "Match whole words only"),
            ("-C, --context N", "Show N lines before and after each match"),
            ("-g, --glob GLOB", "Search only files matching GLOB, such as '*.rs' (repeatable)"),
            ("-m, --max N", "Show at most N matches (default 30)"),
            ("-c, --count", "Show only the number of matches in each file"),
            ("-l, --files", "Show only the names of the files that match"),
            ("-u, --no-ignore", "Also search files that .gitignore ignores"),
            ("--code-only", "Skip tests and documentation"),
        ],
        examples: &["zazzle find 'fn main' src", "zazzle find TODO -g '*.rs' -c", "zazzle find -F 'a.b(c)' -C 2"],
    },
    Page {
        name: "read",
        group: 0,
        brief: "read PATH[:A-B]...",
        summary: "Show files or line ranges, with line numbers",
        usage: &["zazzle read PATH[:A-B]... [OPTIONS]", "zazzle read --symbol Namespace.Type.Member"],
        about: &[
            "Shows each file with a header (line count and sha), then numbered lines. Beyond about 3000 tokens the middle is left out, and the output says which range to read next. PATH:A-B reads lines A to B.",
            "Code files are shown as a terse view: declarations and behaviour, without the sha. Use --source for the code itself and the sha that edit needs.",
        ],
        options: &[
            ("-r, --rev REV", "Read the files as they are at a git revision (also REV:PATH)"),
            ("--source", "Show the code itself and its sha"),
            ("-s, --symbol NAME[#N]", "Show one symbol (the Nth of that name) with its comments and attributes"),
            ("-a, --around N", "Show the lines around line N"),
            ("--ctx N", "How many lines --around shows on each side (default 20)"),
            ("-n, --head N", "Show the first N lines"),
            ("-t, --tail N", "Show the last N lines"),
            ("--anchors", "Show the code with line anchors that edit --at accepts"),
            ("-b, --budget T", "Cap the output at about T tokens (default 3000)"),
            ("--full", "Show everything, with no token cap"),
        ],
        examples: &["zazzle read src/main.rs", "zazzle read src/main.rs:40-80 --source", "zazzle read lib.rs --symbol parse"],
    },
    Page {
        name: "ls",
        group: 0,
        brief: "ls [DIR]",
        summary: "Show a directory tree with file and line counts",
        usage: &["zazzle ls [DIR] [OPTIONS]"],
        about: &["Shows DIR (default: the current folder) as a tree, with how many files each folder holds and how many lines each file has."],
        options: &[
            ("-d, --depth N", "Go N levels deep (default 2)"),
            ("-m, --max N", "Show at most N entries (default 150)"),
            ("-u, --no-ignore", "Also show what .gitignore ignores"),
        ],
        examples: &["zazzle ls", "zazzle ls src -d 4"],
    },
    Page {
        name: "outline",
        group: 0,
        brief: "outline PATH...",
        summary: "List the symbols in files, with their line ranges",
        usage: &["zazzle outline PATH... [OPTIONS]"],
        about: &["Lists each file's functions, types and other symbols as start-end, kind and name, nested by depth. Files without a grammar, and files over 1 MiB, get a line-based outline."],
        options: &[
            ("-d, --depth N", "Show symbols nested up to N levels (default 3)"),
            ("-m, --max N", "Show at most N symbols (default 200)"),
        ],
        examples: &["zazzle outline src/lib.rs", "zazzle outline src -d 1"],
    },
    Page {
        name: "refs",
        group: 0,
        brief: "refs SYMBOL [PATH...]",
        summary: "Find where a symbol is defined and used",
        usage: &["zazzle refs SYMBOL|Type.Member [PATH...] [OPTIONS]"],
        about: &["Shows the definitions first, then each use with the symbol that encloses it. In C# files, a mention in a comment or a string is not counted as a use."],
        options: &[
            ("-g, --glob GLOB", "Look only in files matching GLOB (repeatable)"),
            ("-m, --max N", "Show at most N results (default 50)"),
            ("-u, --no-ignore", "Also look in files that .gitignore ignores"),
            ("--code-only", "Skip tests and documentation"),
        ],
        examples: &["zazzle refs parse_args", "zazzle refs Engine src -g '*.rs'"],
    },
    Page {
        name: "edit",
        group: 0,
        brief: "edit PATH ...",
        summary: "Change a file, if it is still the version you read",
        usage: &[
            "zazzle edit PATH --old TEXT --new TEXT [--all] --expect SHA",
            "zazzle edit PATH --lines A-B --new TEXT (--expect SHA | --at A#xyz-B#xyz)",
            "zazzle edit PATH --after N --new TEXT --expect SHA",
            "zazzle edit PATH --create --new TEXT",
        ],
        about: &[
            "Every edit except --create needs proof that you read the current file: the sha from read --source, or anchors from read --source --anchors. The write is atomic, keeps CRLF line endings, and stays within the working directory, its git work tree and your workspaces. It prints the new sha.",
            "TEXT '-' reads the text from standard input.",
        ],
        options: &[
            ("--old TEXT", "The text to replace; it must occur exactly once unless --all"),
            ("--new TEXT", "The replacement, the new lines or the new file's contents"),
            ("--all", "Replace every occurrence of --old"),
            ("--lines A-B", "Replace lines A to B"),
            ("--after N", "Insert after line N (0 inserts at the top)"),
            ("--expect SHA", "The sha that read --source showed"),
            ("--at A#xyz-B#xyz", "The anchors that read --source --anchors showed"),
            ("--create", "Create a new file"),
        ],
        examples: &["zazzle edit src/a.rs --old 'let x = 1;' --new 'let x = 2;' --expect 3f2a9c1"],
    },
    Page {
        name: "read-again",
        group: 0,
        brief: "read-again r:ID",
        summary: "Show an earlier reply again, or its uncut text",
        usage: &["zazzle read-again r:ID [--full [--lines A-B] [--budget T]]"],
        about: &["Replies from clean, run and proxy start with an id such as [r:ID] and are kept for an hour. read-again shows one again as it was shown, or with --full, the text before anything was cut."],
        options: &[
            ("--full", "Show the uncut text, with line numbers"),
            ("--lines A-B", "With --full, show only lines A to B"),
            ("-b, --budget T", "With --full, cap at about T tokens (default 3000)"),
        ],
        examples: &["zazzle read-again r:7 --full --lines 100-200"],
    },
    Page {
        name: "overview",
        group: 1,
        brief: "overview",
        summary: "Languages, folders, entry points and largest files",
        usage: &["zazzle overview [--repo DIR]"],
        about: &["Describes the git work tree that holds the current folder (or DIR). In a folder of repositories, it lists them."],
        options: &[("-r, --repo DIR", "Describe the repository at DIR")],
        examples: &["zazzle overview"],
    },
    Page {
        name: "find-symbol",
        group: 1,
        brief: "find-symbol NAME",
        summary: "Find declarations by name",
        usage: &["zazzle find-symbol NAME [OPTIONS]"],
        about: &["Finds types, methods and other declarations whose name matches NAME."],
        options: &[
            ("-x, --exact", "Match the name exactly"),
            ("-k, --kind K", "Only declarations of kind K, such as class or method"),
            ("-m, --limit N", "Show at most N results"),
            ("-o, --offset N", "Skip the first N results"),
            ("-r, --repo DIR", "Search the repository at DIR"),
        ],
        examples: &["zazzle find-symbol Parse --kind method"],
    },
    Page {
        name: "changed-since",
        group: 1,
        brief: "changed-since [CURSOR]",
        summary: "Files added, modified or deleted since a cursor",
        usage: &["zazzle changed-since [CURSOR] [--repo DIR]"],
        about: &["Without a cursor, prints one to start from. With one, lists files added (A), modified (M) or deleted (D) since, and a new cursor."],
        options: &[("-r, --repo DIR", "Look at the repository at DIR")],
        examples: &["zazzle changed-since"],
    },
    Page {
        name: "git-log",
        group: 1,
        brief: "git-log [PATH]",
        summary: "Recent commits, of the repository or of one path",
        usage: &["zazzle git-log [PATH] [OPTIONS]"],
        about: &["Shows recent commits, optionally only those that changed PATH."],
        options: &[
            ("-n N", "Show N commits"),
            ("--grep TEXT", "Only commits whose message contains TEXT"),
            ("-r, --repo DIR", "Look at the repository at DIR"),
        ],
        examples: &["zazzle git-log src/main.rs -n 5"],
    },
    Page {
        name: "git-blame",
        group: 1,
        brief: "git-blame PATH",
        summary: "Who last changed each line of a file",
        usage: &["zazzle git-blame PATH [--start N] [--end N]"],
        about: &["Shows the commit and author that last changed each line."],
        options: &[("--start N", "Start at line N"), ("--end N", "Stop at line N")],
        examples: &["zazzle git-blame src/main.rs --start 10 --end 40"],
    },
    Page {
        name: "report-gap",
        group: 1,
        brief: "report-gap TEXT",
        summary: "Tell the maintainers what zazzle could not answer",
        usage: &["zazzle report-gap TEXT [--repo DIR]"],
        about: &["Records a note for zazzle's maintainers about something zazzle could not answer."],
        options: &[("-r, --repo DIR", "The repository the note is about")],
        examples: &["zazzle report-gap 'find-symbol cannot find extension methods'"],
    },
    Page {
        name: "clean",
        group: 2,
        brief: "CMD | zazzle clean",
        summary: "Shrink a command's piped output",
        usage: &["CMD | zazzle clean [OPTIONS]"],
        about: &[
            "Reads standard input and removes what a reader does not need: colours, redraws, blank runs, repeated lines, progress bars, long blobs and over-long lines. grep output is regrouped by file and JSON loses its whitespace. Every removal leaves a marker, and the reply starts with an id for read-again.",
        ],
        options: &[
            ("--as auto|grep|json|text", "Skip detection and treat the input as this kind"),
            ("--lossless", "Only the steps that lose nothing"),
            ("-b, --budget T", "Cap the output at about T tokens (default 8000)"),
            ("--stats", "Add a line with the token counts and the steps used"),
        ],
        examples: &["cargo test 2>&1 | zazzle clean", "grep -rn foo src | zazzle clean --stats"],
    },
    Page {
        name: "run",
        group: 2,
        brief: "run [--] CMD [ARGS...]",
        summary: "Run a command and show its output cleaned",
        usage: &["zazzle run [OPTIONS] [--] CMD [ARGS...]"],
        about: &[
            "Runs CMD in your environment and cleans its output with a profile for cargo, pytest, go, dotnet, jest/vitest, tsc or package installs, then with clean's steps. A footer gives the exit code and time, and zazzle exits with the command's code.",
        ],
        options: &[
            ("--profile NAME|none", "Use this profile instead of the one picked from the command"),
            ("--lossless", "No profile, and only the steps that lose nothing"),
            ("--budget T", "Cap the output at about T tokens (default 8000)"),
            ("--timeout SECS", "Stop the command after SECS seconds (default 3600) and exit 124"),
        ],
        examples: &["zazzle run -- cargo test", "zazzle run --profile none -- make"],
    },
    Page {
        name: "proxy",
        group: 2,
        brief: "proxy -- CMD [ARGS...]",
        summary: "Run an MCP server behind a cleaning proxy",
        usage: &[
            "zazzle proxy [OPTIONS] -- CMD [ARGS...]",
            "zazzle proxy [OPTIONS] --upstream URL [--listen ADDR]",
        ],
        about: &["Sits between an MCP client and another MCP server (stdio, or Streamable HTTP with --upstream) and cleans the text of tool results and resources. zazzle setup adds it for you."],
        options: &[
            ("--lossless", "Only the steps that lose nothing"),
            ("--budget T", "Cap each result at about T tokens"),
            ("--skip TOOL", "Leave this tool's results alone (repeatable)"),
            ("--config FILE", "Read per-tool settings from FILE"),
            ("--log", "Record calls in the activity log"),
            ("--upstream URL", "Proxy the Streamable HTTP server at URL"),
            ("--listen ADDR", "With --upstream, listen on ADDR (such as 127.0.0.1:8808)"),
        ],
        examples: &[
            "zazzle proxy -- npx -y @modelcontextprotocol/server-filesystem ~/src",
            "zazzle proxy --upstream https://mcp.example.com/mcp --listen 127.0.0.1:8808",
        ],
    },
    Page {
        name: "trace",
        group: 2,
        brief: "trace on|off|status",
        summary: "Record everything the MCP proxies send and receive",
        usage: &["zazzle trace [on [DIR] | off | status]"],
        about: &["While on, every proxy writes each request to DIR/MMDD_HHmmss: the request, the response before and after cleaning, and results.json with tokens and timings. Running proxies switch at once."],
        options: &[],
        examples: &["zazzle trace on ~/traces", "zazzle trace off"],
    },
    Page {
        name: "setup",
        group: 3,
        brief: "setup",
        summary: "Set up Claude Code, Claude desktop and Codex",
        usage: &["zazzle setup [--apply | --undo | --off | --on | --uninstall | --check | --verify | --report] [OPTIONS]"],
        about: &[
            "Without an action it changes nothing and prints the plan. It edits only your user-level settings, backs up every file it changes, and a second --apply changes nothing.",
        ],
        options: &[
            ("--apply", "Make the changes in the plan"),
            ("--check", "Check the settings, the hook and zazzle itself, without starting other servers"),
            ("--probe", "With --check, also start every configured server and check it answers"),
            ("--verify", "Use zazzle as an agent would, in this repository"),
            ("--report", "Summarize the activity log"),
            ("--off, --on", "Turn the setup off, keeping its options, and on again"),
            ("--undo", "Remove everything setup added"),
            ("--uninstall", "--undo, and also stop zazzled's own LaunchAgent"),
            ("--client NAME", "Only claude-code, claude-desktop or codex (repeatable)"),
            ("--repo DIR", "The folder the Claude desktop app should use"),
            ("--workspace DIR", "Let edits reach every repository below DIR (repeatable)"),
            ("--no-hook", "Leave out the hook that sends shell searches, reads and builds to zazzle"),
            ("--hook-edits", "Also send the client's own edits through zazzle"),
            ("--no-wrap", "Leave the client's other MCP servers alone"),
        ],
        examples: &["zazzle setup --workspace ~/dev", "zazzle setup --apply --workspace ~/dev", "zazzle setup --check"],
    },
    Page {
        name: "status",
        group: 3,
        brief: "status",
        summary: "Show the daemon's version, uptime and cache",
        usage: &["zazzle status"],
        about: &["zazzle's work is done by a background daemon, zazzled. status shows the running daemon's state."],
        options: &[],
        examples: &["zazzle status"],
    },
    Page {
        name: "languages",
        group: 3,
        brief: "languages",
        summary: "List or download the language modules",
        usage: &["zazzle languages [jit [on|off] | fetch [NAME...]]"],
        about: &["Grammars for outline, refs and the terse views are downloaded as language modules. Without arguments, lists the installed and missing ones."],
        options: &[
            ("jit on|off", "Download a module the first time it is needed, or not"),
            ("fetch [NAME...]", "Download the missing modules, or the ones named"),
        ],
        examples: &["zazzle languages", "zazzle languages fetch python go"],
    },
    Page {
        name: "version",
        group: 3,
        brief: "version",
        summary: "Show the version of the running daemon",
        usage: &["zazzle version", "zazzle --version"],
        about: &[],
        options: &[],
        examples: &[],
    },
    Page {
        name: "help",
        group: 3,
        brief: "help [COMMAND]",
        summary: "Show this overview, or one command's options",
        usage: &["zazzle help [COMMAND]", "zazzle help --all", "zazzle COMMAND --help"],
        about: &[],
        options: &[],
        examples: &["zazzle help find", "zazzle read --help"],
    },
    Page {
        name: "git",
        group: 4,
        brief: "git ...",
        summary: "git with a commit flow and message editor",
        usage: &["zazzle git [message FILE | commit [--no-add] [OPTIONS] | GIT ARGS...]"],
        about: &["zazzle git has its own help: run zazzle git --help."],
        options: &[],
        examples: &["zazzle git commit -m 'Fix the parser'"],
    },
    Page {
        name: "free-disk-space",
        group: 4,
        brief: "free-disk-space ...",
        summary: "Find and reclaim disk space on macOS",
        usage: &["zazzle free-disk-space [COMMAND] [OPTIONS]"],
        about: &["free-disk-space has its own help: run zazzle free-disk-space --help."],
        options: &[],
        examples: &[],
    },
    Page {
        name: "fix-network",
        group: 4,
        brief: "fix-network ...",
        summary: "Repair Wi-Fi, DNS and VPN problems on macOS",
        usage: &["zazzle fix-network [COMMAND] [OPTIONS]"],
        about: &["fix-network has its own help: run zazzle fix-network --help."],
        options: &[],
        examples: &[],
    },
    Page {
        name: "zazz",
        group: 4,
        brief: "zazz ...",
        summary: "Write, test and run sandboxed scripts",
        usage: &["zazz COMMAND [ARGS...]", "zazzle zazz COMMAND [ARGS...]"],
        about: &["zazz has its own help: run zazz help."],
        options: &[],
        examples: &[],
    },
];

// Splits text into lines of at most `width` characters, breaking at spaces.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let last = lines.last_mut().expect("lines starts with one line");
        if last.is_empty() {
            last.push_str(word);
        } else if last.chars().count() + 1 + word.chars().count() <= width {
            last.push(' ');
            last.push_str(word);
        } else {
            lines.push(word.to_string());
        }
    }
    lines
}

// Appends a paragraph wrapped to the page width, every line indented.
fn paragraph(out: &mut String, indent: usize, text: &str) {
    for line in wrap(text, WIDTH - indent) {
        out.push_str(&format!("{:indent$}{line}\n", ""));
    }
}

// Appends a two-column row: the term at the left, its wrapped description from column LEFT + 2.
fn row(out: &mut String, term: &str, text: &str) {
    let lines = wrap(text, WIDTH - LEFT - 2);
    let mut lines = lines.iter();
    if term.chars().count() <= LEFT - 2 {
        out.push_str(&format!("  {term:<w$}{}\n", lines.next().map_or("", |s| s), w = LEFT));
    } else {
        out.push_str(&format!("  {term}\n"));
    }
    for line in lines {
        out.push_str(&format!("{:w$}{line}\n", "", w = LEFT + 2));
    }
}

// Returns the overview that bare `zazzle` and `zazzle help` print.
pub fn overview() -> String {
    let mut out = format!("zazzle {VERSION}: read, search and edit code with short, focused output\n\n");
    out.push_str(
        "Usage: zazzle COMMAND [ARGUMENTS]\n       zazzle help COMMAND     all of a command's options\n",
    );
    for (i, group) in GROUPS.iter().enumerate() {
        out.push_str(&format!("\n{group}:\n"));
        for p in PAGES.iter().filter(|p| p.group == i) {
            row(&mut out, p.brief, p.summary);
        }
    }
    out.push_str("\nOptions for every command:\n");
    for (term, text) in COMMON {
        row(&mut out, term, text);
    }
    out.push('\n');
    paragraph(
        &mut out,
        0,
        "Code files are read as a terse view of declarations and behaviour; read --source shows the code and the sha that edit needs.",
    );
    out.push_str("\nRun `zazzle help COMMAND` for a command's options and examples.");
    out
}

// Returns the overview agents get: only the agent commands, and a pointer to the full list for people.
pub fn agent_overview() -> String {
    let mut out = format!("zazzle {VERSION}: read, search and edit code with short, focused output\n\n");
    for name in AGENT {
        let p = PAGES.iter().find(|p| p.name == *name).expect("every agent command has a page");
        row(&mut out, p.brief, p.summary);
    }
    out.push_str("\nzazzle help COMMAND shows a command's options.\nhumans: zazzle help --all");
    out
}

// Returns the help page of one command, or an error naming the unknown command.
pub fn page(name: &str) -> Result<String, String> {
    let Some(p) = PAGES.iter().find(|p| p.name == name) else {
        return Err(format!("no command '{name}'; known: {}", AGENT.join(" ")));
    };
    let mut out = String::new();
    paragraph(&mut out, 0, &format!("zazzle {}: {}", p.name, p.summary));
    out.push_str("\nUsage:\n");
    for u in p.usage {
        out.push_str(&format!("  {u}\n"));
    }
    for text in p.about {
        out.push('\n');
        paragraph(&mut out, 0, text);
    }
    if !p.options.is_empty() {
        out.push_str("\nOptions:\n");
        for (term, text) in p.options {
            row(&mut out, term, text);
        }
    }
    if !p.examples.is_empty() {
        out.push_str("\nExamples:\n");
        for e in p.examples {
            out.push_str(&format!("  {e}\n"));
        }
    }
    if p.group < 3 {
        out.push('\n');
        paragraph(&mut out, 0, "Also: --budget T and --disable-session-deduping work with every command.");
    }
    Ok(out.trim_end().to_string())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks agent help names every agent command and no command for people.
    #[test]
    fn test_agent_overview_has_only_agent_commands() {
        let text = agent_overview();
        for p in PAGES {
            let listed = text.lines().any(|l| l.trim_start().starts_with(p.brief));
            assert_eq!(listed, is_agent(p.name), "{}", p.name);
        }
        assert!(text.ends_with("humans: zazzle help --all"));
        assert!(page("nope").unwrap_err().split("known: ").nth(1).expect("list").split(' ').all(is_agent));
    }

    // Checks no line of any help text is wider than the page.
    #[test]
    fn test_help_fits_width() {
        let mut all = overview();
        for p in PAGES {
            all.push('\n');
            all.push_str(&page(p.name).expect("page"));
        }
        for line in all.lines().filter(|l| !l.starts_with("  zazzle ") && !l.contains(" | zazzle clean")) {
            assert!(line.chars().count() <= WIDTH, "too wide: {line}");
        }
    }

    // Checks the overview lists every command once, under its group, with a one-line summary.
    #[test]
    fn test_help_overview_lists_commands() {
        let o = overview();
        assert!(o.starts_with(&format!("zazzle {VERSION}: ")));
        for g in GROUPS {
            assert!(o.contains(&format!("\n{g}:\n")), "{g}");
        }
        for p in PAGES {
            assert_eq!(o.matches(&format!("\n  {} ", p.brief)).count(), 1, "{}", p.name);
            assert!(p.summary.chars().count() <= WIDTH - LEFT - 2, "summary of {} wraps", p.name);
        }
        assert!(o.contains("\n  find PATTERN [PATH...]    Search files for a regular expression\n"));
    }

    // Checks a page has its usage, options and examples, and a long term goes on a line of its own.
    #[test]
    fn test_help_page() {
        let f = page("find").expect("find");
        assert!(f.starts_with("zazzle find: Search files for a regular expression\n"));
        assert!(f.contains("\nUsage:\n  zazzle find PATTERN [PATH...] [OPTIONS]\n"));
        assert!(f.contains("\n  -i, --ignore-case         Ignore case\n"));
        assert!(f.contains("\nExamples:\n  zazzle find 'fn main' src\n"));
        assert!(f.ends_with("\nAlso: --budget T and --disable-session-deduping work with every command."));
        assert!(page("clean").expect("clean").contains("\n  --as auto|grep|json|text  Skip detection"));
        assert!(overview()
            .contains("\n  --disable-session-deduping\n                            Send output again"));
        assert!(!page("git").expect("git").contains("--budget"));
        assert_eq!(
            page("nope").unwrap_err(),
            "no command 'nope'; known: read outline find refs find-symbol edit ls run clean read-again"
        );
    }

    // Checks wrapping breaks at spaces and keeps a word longer than the width whole.
    #[test]
    fn test_help_wrap() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh x", 4), vec!["abcdefgh", "x"]);
        assert_eq!(wrap("", 4), vec![""]);
    }
}
