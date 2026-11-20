// Help for people at a terminal: the command overview and one page per command, wrapped to 80 columns.
use super::VERSION;

const WIDTH: usize = 80;
const LEFT: usize = 26;
const GROUPS: [&str; 5] = [
    "Read, search and edit files",
    "Repository structure (from csharp-indexer's index)",
    "Command output",
    "Setup and the daemon",
    "Other tools",
];
// The commands agents see, the one list behind agent help, the MCP tool list and the agent instructions; every other command is for people and shows only in `help --all`.
pub const AGENT: &[&str] = &[
    "read",
    "outline",
    "find",
    "refs",
    "find-symbol",
    "edit",
    "replace",
    "ls",
    "stat",
    "diff",
    "run",
    "clean",
    "read-again",
    "call",
    "get-rules",
    "brief",
    "prompt",
    "investigate",
];

// True when agents are shown the command.
pub fn is_agent(cmd: &str) -> bool {
    AGENT.contains(&cmd)
}

const COMMON: &[(&str, &str)] = &[
    ("--llm TEXT", "Hand the output to the local model with this instruction and print its answer (zazzle agent llm)"),
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
            "Shows the literal source: a header (line count and sha, which edit needs), then numbered lines. PATH:A-B reads lines A to B; --symbol reads one declaration whole, with its comments and attributes.",
            "Past the budget (default 6000 tokens, at most 16000) it shows the lines from the start of the selection that fit, marks the header INCOMPLETE and ends with the omitted range and the read that continues it; nothing in the middle is dropped.",
            "--terse is an opt-in lossy summary: it folds imports, comments, logging, long strings and lists, and has no sha. Do not use it to answer questions about exact behaviour.",
        ],
        options: &[
            ("-r, --rev REV", "Read the files as they are at a git revision (also REV:PATH)"),
            ("--terse", "Show the lossy terse summary instead of the source"),
            ("--source", "The source (the default; kept for compatibility)"),
            ("-s, --symbol NAME[#N]", "Show one symbol (the Nth of that name) with its comments and attributes"),
            ("-a, --around N", "Show the lines around line N"),
            ("--ctx N", "How many lines --around shows on each side (default 20)"),
            ("-n, --head N", "Show the first N lines"),
            ("-t, --tail N", "Show the last N lines"),
            ("--anchors", "Show the code with line anchors that edit --at accepts"),
            ("-b, --budget T", "Cap the output at about T tokens (default 6000, at most 16000)"),
            ("--full", "Use the largest budget, 16000 tokens"),
        ],
        examples: &["zazzle read src/main.rs:40-80", "zazzle read lib.rs --symbol parse", "zazzle read a.rs b.rs:10-30"],
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
        name: "stat",
        group: 0,
        brief: "stat PATH...",
        summary: "Show whether paths exist, their kind, size and time",
        usage: &["zazzle stat PATH... [--hash]"],
        about: &["Prints one line per path: its kind (file, dir, symlink or other; a symlink is not followed), size in bytes and modification time in UTC, or `missing`. --hash adds each file's sha, the one read shows and edit --expect takes."],
        options: &[("--hash", "Add each file's sha")],
        examples: &["zazzle stat Cargo.toml src --hash"],
    },
    Page {
        name: "call",
        group: 0,
        brief: "call JSON",
        summary: "Run JSON requests: read, search, list, stat, diffs",
        usage: &["zazzle call 'JSON'", "zazzle call - < requests.json"],
        about: &[
            "JSON is one request object, or an array of them answered in order. - (or no argument) reads it from standard input. Each request names an op: read, search (or find), list (or ls), stat, outline or diff_since. Every reply has ok; a failed one carries error, and call then exits 1.",
            "Replies carry an etag. Within a session (ZAZZLE_SESSION, or the MCP session) a repeated read, search, list or outline returns {unchanged:true, ref}, a changed whole-file read returns only hunks (apply from the last at to the first), read takes if_none_match and diff_since gives the hunks since an etag the session was sent. fresh:true sends the content again. Without a session nothing is remembered.",
            "Fields. read: path ranges [[a,b]] max_lines max_bytes if_none_match fresh. search: pattern literal ignore_case paths glob context max_matches files_only fields (line col text) hidden no_ignore fresh. list: path glob depth long max_entries cursor hidden no_ignore fresh. stat: path hash. outline: paths max_symbols fresh. diff_since: path etag.",
        ],
        options: &[],
        examples: &[
            "zazzle call '{\"op\":\"read\",\"path\":\"src/main.rs\",\"ranges\":[[1,40]]}'",
            "zazzle call '{\"op\":\"search\",\"pattern\":\"TODO\",\"glob\":[\"*.rs\"],\"context\":1}'",
            "zazzle call '[{\"op\":\"list\",\"path\":\"src\",\"cursor\":500},{\"op\":\"stat\",\"path\":\"Cargo.toml\",\"hash\":true}]'",
        ],
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
        about: &["Shows the definitions first, then each use with the symbol that encloses it. A mention in a comment or a string is not counted as a use; in C#, the indexer's Roslyn answer comes first and stays exact within the paths and globs given. SYMBOL is a literal name; --also adds more, each answered under its own `== NAME` line. To read a known symbol's source, use read --symbol instead."],
        options: &[
            ("--also NAME", "Also answer for NAME (repeatable)"),
            ("-g, --glob GLOB", "Look only in files matching GLOB (repeatable)"),
            ("-m, --max N", "Show at most N results (default 50)"),
            ("-u, --no-ignore", "Also look in files that .gitignore ignores"),
            ("--code-only", "Skip tests and documentation"),
        ],
        examples: &["zazzle refs parse_args", "zazzle refs Engine src -g '*.rs'"],
    },
    Page {
        name: "replace",
        group: 0,
        brief: "replace PATTERN TEXT",
        summary: "Replace a regular expression across files",
        usage: &["zazzle replace PATTERN REPLACEMENT [PATH...] [OPTIONS]"],
        about: &["Replaces every match in the files below PATH (default: the current folder) that .gitignore does not ignore; $1 in REPLACEMENT is the first group. Each file is written only if it did not change since it was read, and only inside the folders edit may write."],
        options: &[
            ("-g, --glob GLOB", "Only files matching GLOB (repeatable)"),
            ("-n, --dry-run", "Show the changed lines and write nothing"),
            ("-i, --ignore-case", "Ignore case"),
            ("-F, --fixed", "PATTERN and REPLACEMENT are plain text"),
            ("-u, --no-ignore", "Also files that .gitignore ignores"),
        ],
        examples: &["zazzle replace 'old_(\\w+)' 'new_$1' -g '*.rs' --dry-run", "zazzle replace -F 'a.b' 'a::b' src"],
    },
    Page {
        name: "diff",
        group: 0,
        brief: "diff [RANGE] [PATH...]",
        summary: "Show what changed, file stats first",
        usage: &["zazzle diff [BASE[..HEAD|...HEAD]] [PATH...] [OPTIONS]"],
        about: &["With no range, compares the working tree with HEAD. Lists each file's added and removed lines first, then the hunks, each labelled with the symbol it is in. Lock files, generated code and files .ignore matches are listed as skipped unless named. When the budget runs out, a footer names the files to read next."],
        options: &[
            ("--names", "Show only the file stats"),
            ("-C, --context N", "Lines of context around each change (default 3)"),
        ],
        examples: &["zazzle diff", "zazzle diff main...HEAD --names", "zazzle diff HEAD~1 src/a.rs"],
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
            "Every edit except --create needs proof that you read the current file: the sha that read shows, or anchors from read --anchors. The write is atomic, keeps CRLF line endings, and stays within the working directory, its git work tree and your workspaces. It prints the new sha.",
            "TEXT '-' reads the text from standard input.",
        ],
        options: &[
            ("--old A --new B ..", "Several pairs apply in order in one write; none is written if one fails"),
            ("--old TEXT", "The text to replace; it must occur exactly once unless --all"),
            ("--new TEXT", "The replacement, the new lines or the new file's contents"),
            ("--all", "Replace every occurrence of --old"),
            ("--lines A-B", "Replace lines A to B"),
            ("--after N", "Insert after line N (0 inserts at the top)"),
            ("--expect SHA", "The sha that read showed"),
            ("--at A#xyz-B#xyz", "The anchors that read --anchors showed"),
            ("--create", "Create a new file"),
        ],
        examples: &["zazzle edit src/a.rs --old 'let x = 1;' --new 'let x = 2;' --expect 3f2a9c1"],
    },
    Page {
        name: "get-rules",
        group: 0,
        brief: "get-rules PATH..",
        summary: "Rules and skills for paths, once a session",
        usage: &["zazzle get-rules PATH.. [--purpose NAME].. [--name-only] [--skill-bodies] [--again]", "zazzle agent get-rules PATH.."],
        about: &["Lists the AGENTS.md, CLAUDE.md, imported, scoped-rule and skill files that apply to the paths, each once. In a session it leaves out what the session was already sent, by get-rules or after a read or edit, and names it in one line; a changed file, a new purpose's sections, or a session reset after compaction send it again."],
        options: &[
            ("--purpose NAME", "Only the sections marked for NAME (repeatable)"),
            ("--name-only", "Only the file names"),
            ("--skill-bodies", "Whole skill files, not their descriptions"),
            ("--again", "Send everything, even what the session has"),
        ],
        examples: &["zazzle get-rules src/api src/web --purpose review"],
    },
    Page {
        name: "brief",
        group: 0,
        brief: "brief INTENT..",
        summary: "The rules and skills a task needs",
        usage: &["zazzle brief INTENT.. [--path PATH]..", "zazzle agent brief INTENT.. [--path PATH].."],
        about: &["Before a task: the local model picks, from every rule section and skill that applies to the paths (default: here), the ones the intent needs; they print verbatim, with line ranges, and the rest are named in one line. Needs a downloaded model (zazzle agent llm)."],
        options: &[("--path PATH", "A file or folder the work touches (repeatable)")],
        examples: &["zazzle brief 'add a subcommand to git-tools' --path src/macos/git-tools"],
    },
    Page {
        name: "investigate",
        group: 0,
        brief: "investigate QUESTION..",
        summary: "Local model answers with checked citations",
        usage: &["zazzle investigate QUESTION.. [--path P].. [--symbol S].. [--steps N] [--seconds N]", "zazzle investigate QUESTION.. --adaptive [--agents A,B..] [--prose] [--json]", "zazzle investigate QUESTION.. [--benchmark-model M] [--benchmark-effort E] [--review-model M] [--review-effort E]", "zazzle agent investigate QUESTION.."],
        about: &["Hands a question that needs several files to the local model. Starting from the symbols' references, it reads, outlines and searches inside the paths (default: here) for up to N turns, then answers. Each cited path:line is checked against the file and quoted literally; bad citations and what it could not establish are listed, and a run that stopped early says INCOMPLETE. The rules for the files it reads reach the model apart from the evidence, and you get those for the cited files. For one known symbol or a short lookup, read or refs is cheaper. Needs a downloaded model (zazzle agent llm)."],
        options: &[
            ("-p, --path P", "A folder or file to stay inside (repeatable)"),
            ("-s, --symbol S", "A name to start from (repeatable)"),
            ("--steps N", "Model turns, 1 to 12 (default 6)"),
            ("--seconds N", "Time limit, waiting for the model included (default 90); a benchmark and a review get as long again"),
            ("--adaptive", "Plan the question into parts, run each on the cheapest agent and effort that will do it, escalate unsettled parts with what they established"),
            ("--agents A,B..", "The adaptive ladder, cheapest first: MODEL, claude:MODEL or codex:MODEL, each optionally @EFFORT (default: installed local models, each at low then high)"),
            ("--prose", "Adaptive: the cheapest agent adds a short summary that keeps the citations"),
            ("--json", "The structured result: status, confidence, findings with evidence, gaps, escalation, usage"),
            ("--benchmark-model M, --benchmark-effort E", "Also answer with that agent alone and compare state, findings, calls, time, tokens and cost"),
            ("--review-model M, --review-effort E", "Have that agent judge each result's quality and each finding"),
        ],
        examples: &["zazzle investigate 'Where is the idempotency key validated?' --path src/Api --symbol IdempotencyFilter", "zazzle investigate 'Why are blank keys accepted, and should they be?' --adaptive --agents qwen3.5-4b,qwen3.5-9b,claude:sonnet@medium --review-model claude:opus"],
    },
    Page {
        name: "prompt",
        group: 0,
        brief: "prompt TEXT..",
        summary: "Ask the local model; for agents",
        usage: &["zazzle prompt [--effort low|medium|high] TEXT.."],
        about: &["Asks the local model and prints its whole answer, outside any chat. Never downloads a model; zazzle agent prompt adds to this terminal's chat, reads standard input and downloads one. To shrink another command's output, give that command --llm TEXT instead."],
        options: &[("--effort E", "low (no thinking), medium or high; default: the saved one")],
        examples: &["zazzle prompt --effort low 'Is this a valid semver range: ^1.2.x?'", "zazzle run --llm 'Only the failing tests and their errors' -- cargo test"],
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
        brief: "find-symbol NAME...",
        summary: "Find declarations by name",
        usage: &["zazzle find-symbol NAME... [OPTIONS]"],
        about: &["Finds types, methods and other declarations whose name matches NAME, a literal name (not a pattern); several names are answered together, each under its own `== NAME` line. When you already know the symbol, read --symbol returns its source in one step."],
        options: &[
            ("-p, --path DIR|GLOB", "Only declarations in files under DIR or matching GLOB (repeatable)"),
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
        name: "flow",
        group: 4,
        brief: "flow SUBCOMMAND ..",
        summary: "Repository workflow: verify, write, deliver",
        usage: &["zazzle flow preview|init|verify|tidy|coverage|audit|update|write|deliver|test-first|ci ..", "zazzle flow help"],
        about: &["Opt in per repository with .zazzle/flow.json (zazzle flow init). verify runs the repository's own checks that apply to the change and keeps a receipt pinned to the exact tree; write has a cheap writer draft the commit message, changelog entry or pull request text from settled facts and validates it; deliver stages exactly the named paths, commits, pushes and opens or updates the pull request, safe to repeat; test-first has one agent write a test that fails for the stated reason and another make it pass; ci waits for the pull request's checks on its head and reports once. Runs in your environment."],
        options: &[
            ("preview", "What applies here and on whose authority, changing nothing"),
            ("verify [--only NAME]..", "Run the applicable checks; the receipt names the tree"),
            ("tidy", "Formatters' and linters' fixes; leftover lint to the local agent"),
            ("coverage [--author A]", "Uncovered lines and branches of the change; tests written for them"),
            ("audit [--fix]", "Dependency audits; advisories listed, fixes run where they exist"),
            ("update", "Merge the target in; lockfiles regenerated, conflicts to the local agent"),
            ("write KIND --intent T", "commit, changelog, pr or docs; --compat gives the compatibility decision"),
            ("deliver --paths P.. -m M", "--push, --pr, --draft; reuses what a retry finds done"),
        ],
        examples: &["zazzle flow verify", "zazzle flow deliver --paths src -m 'Fix the overflow' --pr --draft"],
    },
    Page {
        name: "review",
        group: 4,
        brief: "review [--into BRANCH]",
        summary: "Multi-viewpoint review that fixes what it finds",
        usage: &["zazzle review [--into BRANCH] [--level L] [--viewpoints V,..] [--intent TEXT] [--read-only] [--bench] [--json]"],
        about: &["Reviews the change at the level of the branch it goes into (feature, development, staging, production): zazzle picks the viewpoints, a local planner briefs each, independent reviewers report findings with evidence, and a final reviewer deduplicates and challenges them. Unless --read-only, confirmed findings are fixed, verified and re-reviewed before the report; what is left needs your decision or access."],
        options: &[
            ("--into BRANCH", "Review at that branch's level, against it"),
            ("--reviewers A,..", "Reviewer agents (MODEL, claude:MODEL, codex:MODEL, @EFFORT)"),
            ("--final A, --fix-agent A", "The final reviewer and the fixer"),
            ("--bench", "Also answer the benchmark questions, phase by phase"),
        ],
        examples: &["zazzle review --into dev --intent 'reject blank idempotency keys'"],
    },
    Page {
        name: "setup",
        group: 3,
        brief: "setup",
        summary: "Set up Claude Code, Claude desktop and Codex",
        usage: &["zazzle setup [--apply | --undo | --off | --on | --uninstall | --check | --verify | --report | --explain] [OPTIONS]"],
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
            ("--uninstall", "--undo, and also stop daemon's own LaunchAgent"),
            ("--client NAME", "Only claude-code, claude-desktop or codex (repeatable)"),
            ("--repo DIR", "The folder the Claude desktop app should use"),
            ("--workspace DIR", "Let edits reach every repository below DIR (repeatable)"),
            ("--no-hook", "Leave out the hook that sends shell searches, reads and builds to zazzle"),
            ("--hook-edits", "Also send the client's own edits through zazzle"),
            ("--explain", "Describe what ~/.zazzle/policy.toml controls and what it holds now"),
            ("--set KEY=VALUE", "Change a policy key (with --apply, write it and apply; alone, preview)"),
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
        about: &["zazzle's work is done by a background daemon, daemon. status shows the running daemon's state."],
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
        usage: &[
            "zazzle git [message FILE | commit [--no-add] [OPTIONS] | GIT ARGS...]",
            "zazzle git-tools ...     (the same program, under its own name)",
        ],
        about: &["zazzle git has its own help: run zazzle git --help."],
        options: &[],
        examples: &["zazzle git commit -m 'Fix the parser'"],
    },
    Page {
        name: "agent",
        group: 4,
        brief: "agent CMD ...",
        summary: "Rule files for a path; local open models",
        usage: &[
            "zazzle agent get-rules PATH [--purpose NAME]... [--name-only] [--skill-bodies]",
            "zazzle agent check [PATH] [--json] | explain PATH [--purpose NAME]... [--json]",
            "zazzle agent llm [--model NAME] [--effort low|medium|high] [--remove NAME]",
            "zazzle agent status [--json] [--verify] | prune [--apply]",
            "zazzle agent chat",
            "zazzle agent prompt [--model NAME] [--effort E] [--system TEXT] [--max-tokens N] [TEXT.. | - | /COMMAND]",
        ],
        about: &[
            "get-rules prints the rule and skill files that apply to a path: see zazzle help get-rules. check reports broken @imports, paths: globs that match nothing and bad skill front matter (exit 1 when it finds any); explain says why each rule file near a path applies or not.",
            "llm lists the open models, or downloads one and makes it and an effort the default; run zazzle agent llm --help. ai-agent is downloaded on first use, and each model on first use. status shows the default model, the downloaded ones and their disk use; prune removes every model but the default (it only lists them without --apply).",
            "chat talks with a local model in this terminal, at low effort unless /effort changes it: /help lists the commands (/effort, /model, /clear, /new, /status, /quit), Ctrl+K starts a new conversation and Ctrl+C leaves. Each terminal has its own conversation, which the daemon keeps: chat again in the same terminal to resume it.",
            "prompt is that chat for one message: it adds the message to this terminal's conversation and prints the answer, without the banner; TEXT starting with / runs a chat command (e.g. /clear). The daemon keeps the model loaded between messages and stops it after ZAZZLE_LLM_IDLE seconds unused (default 600).",
        ],
        options: &[],
        examples: &[
            "zazzle agent get-rules src/main.rs --name-only",
            "zazzle agent llm --model qwen3.5-9b --effort high",
            "git diff | zazzle agent prompt 'Review this diff:' -",
            "zazzle agent prompt /clear",
            "zazzle agent check",
        ],
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
        name: "daemon",
        group: 3,
        brief: "daemon CMD",
        summary: "Start, stop, check or install the daemon",
        usage: &[
            "zazzle daemon start | stop | restart",
            "zazzle daemon status [--json]",
            "zazzle daemon install | uninstall",
            "zazzle daemon mcp | serve | version",
        ],
        about: &[
            "The daemon answers every other zazzle command; any command starts it when none runs. status shows the running and installed versions and, on macOS, the LaunchAgent; it exits 3 when none runs. install makes launchd start it at login.",
        ],
        options: &[],
        examples: &["zazzle daemon status", "zazzle daemon restart"],
    },
    Page {
        name: "tools",
        group: 3,
        brief: "tools [--json]",
        summary: "List the installed tools and their versions",
        usage: &["zazzle tools [--json]"],
        about: &[
            "Every tool runs as zazzle TOOL ARGS from the tools' folder. tools asks each for its version and flags one that is missing or from another release; it exits 1 then.",
        ],
        options: &[],
        examples: &["zazzle tools"],
    },
    Page {
        name: "ai-agent",
        group: 4,
        brief: "ai-agent ...",
        summary: "Run open models locally (zazzle agent llm|prompt)",
        usage: &["zazzle ai-agent [COMMAND] [OPTIONS]"],
        about: &["ai-agent is downloaded on first use; run zazzle ai-agent --help."],
        options: &[],
        examples: &[],
    },
    Page {
        name: "ai-agent-rules",
        group: 4,
        brief: "ai-agent-rules ...",
        summary: "The rule lister behind zazzle agent get-rules",
        usage: &["zazzle ai-agent-rules [COMMAND] [OPTIONS]"],
        about: &["ai-agent-rules has its own help: run zazzle ai-agent-rules --help."],
        options: &[],
        examples: &[],
    },
    Page {
        name: "csharp-indexer",
        group: 4,
        brief: "csharp-indexer ...",
        summary: "The C# index behind the structure commands",
        usage: &["zazzle csharp-indexer [COMMAND] [OPTIONS]"],
        about: &["csharp-indexer has its own help: run zazzle csharp-indexer --help."],
        options: &[],
        examples: &[],
    },
    Page {
        name: "cyberspace-generator",
        group: 4,
        brief: "cyberspace-generator ...",
        summary: "Validate and maintain a repository's .dev files",
        usage: &["zazzle cyberspace-generator [COMMAND] [OPTIONS]"],
        about: &["cyberspace-generator has its own help: run zazzle cyberspace-generator --help."],
        options: &[],
        examples: &[],
    },
    Page {
        name: "zazz",
        group: 4,
        brief: "zazz ...",
        summary: "Write, test and run sandboxed scripts",
        usage: &["zazzle zazz COMMAND [ARGS...]"],
        about: &["zazz has its own help: run zazzle zazz help."],
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
        "read shows the literal source with the sha that edit needs; --terse is an opt-in lossy summary.",
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
            "no command 'nope'; known: read outline find refs find-symbol edit replace ls stat diff run clean read-again call get-rules brief prompt investigate"
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
