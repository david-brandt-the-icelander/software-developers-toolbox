# zazzle / zazzled

A file tool for agents: `zazzle` replaces `grep`, `cat`, `sed -n`, `find`, `ls` and `sed -i` with commands whose output is short, capped, ranked and deduplicated per session.

- `zazzle` (`macos/c/zazzle/`): a tiny C program with no libc on Linux (raw syscalls); on macOS it links libSystem but still uses raw syscalls for all I/O. It sends its arguments, working-directory descriptor and standard streams to the daemon and exits with the daemon's status byte.
- `zazz`: the same invoker under another name, for the zazz script commands below; installed scripts are symlinks to it.
- `zazzled` (this crate): the long-lived daemon. `zazzled serve` listens on `~/.zazzle/sock`; `zazzled mcp` is an MCP stdio server in front of the same daemon (no process spawn per call); `zazzled run CMD..` runs one command in-process; `zazzled proxy` is the MCP proxy that `zazzle proxy` execs ([MCP proxy](#mcp-proxy)); `zazzled install` / `uninstall` manage the macOS LaunchAgent. A daemon whose binary an upgrade replaced (the program the LaunchAgent names, else the Homebrew opt path of its own binary) retires before serving anything more, so `zazzle` never answers from the old version: on its own socket it removes the socket and answers queued requests with a restart reply, which `zazzle` resends to a new daemon; under launchd it exits and launchd starts the new binary. A daemon from before that existed (1.2.19 and older) cannot retire itself: `zazzled restart` stops whichever daemon is running and points a LaunchAgent that names another program at the installed one, `zazzled mcp` does the same at start for a daemon older than itself, and `zazzled --version` says when the running daemon is another version.

## Setup

```bash
brew install zazzcat/z/zazzle         # or build: cd macos/rust && cargo build --release -p zazzled && cd ../c/zazzle && make
zazzled install                       # macOS: launchd starts the daemon on demand (elsewhere zazzle starts it itself); writes the Homebrew opt path, so upgrades keep working
zazzle setup --workspace ~/dev  # the plan for Claude Code, the Claude desktop app and Codex; nothing changes yet
zazzle setup --apply --workspace ~/dev
zazzle setup --check                   # after restarting the apps (and trusting the hook in Codex)
zazzle setup --verify --repo ~/dev/api # uses zazzle like an agent would, from inside a repository and from elsewhere
zazzle setup --report                  # what agents actually sent through zazzle in the last day, and the bypasses
zazzle setup --off                     # zazzle out of the way (for a benchmark); `--on` puts the same setup back
zazzle setup --uninstall               # undo everything and stop zazzled's agent, then `brew uninstall zazzle`
```

Tell an agent "run `zazzle setup` and follow it": the report says what it found, what it would change and when to ask you. Any other MCP client works with `{"mcpServers":{"zazzle":{"command":"zazzled","args":["mcp"]}}}`; the working directory of the server is the root for relative paths. Edits may write in the working directory and the git work tree holding it, in the roots an MCP client names (`roots/list`), and in your workspaces (`~/.zazzle/workspaces`, one folder per line, or `ZAZZLE_WORKSPACES`), never in your home folder or `/` as such. Its MCP handshake gives agents short instructions for using it (`instructions`, about 500 characters, since some clients repeat them for every tool), and `tools/list` offers the `zazzle_zazz_*` tools only once a zazz script is registered or `ZAZZLE_MCP_ZAZZ=1` (`0` hides them).

## Configuring clients: `zazzle setup`

`zazzle setup` (`zazzled setup`, its own process) edits only user-level settings, never a repository's shared `.mcp.json` or `.codex/config.toml`. One setup serves every repository: Claude Code and Codex start zazzle in the folder a session opens, `--repo` gives the desktop app a folder (which may hold several repositories), and `--workspace DIR` (repeatable; `--repo` counts too) lets edits reach every repository below DIR wherever a chat starts.

| client | file | zazzle entry |
|---|---|---|
| Claude Code (terminal, VS Code, desktop app's Code tab) | `~/.claude.json` | `zazzled mcp` in user scope, so every project gets it, started in the project's folder; instructions in `~/.claude/CLAUDE.md`; unless `--no-hook`, `zazzled hook --client claude-code` in `~/.claude/settings.json` (Bash, Grep, Glob, Read and the edit tools) |
| Claude desktop app (chat) | `~/Library/Application Support/Claude/claude_desktop_config.json` | the app starts servers in `/`, so with `--repo DIR` the entry is `/bin/sh -c "cd DIR && exec zazzled mcp"`; DIR may be a folder above several repositories (see C# structure) |
| Codex (CLI, VS Code extension, desktop app) | `$CODEX_HOME/config.toml` (`~/.codex`) | `[mcp_servers.zazzle]`, comments and layout kept; instructions in `~/.codex/AGENTS.md`; unless `--no-hook`, `zazzled hook --client codex` in `~/.codex/hooks.json` (shell commands and `apply_patch`), which Codex runs once you trust it (`/hooks` in the CLI, Settings > Hooks in the app) |

- Other servers: a stdio server becomes `zazzled proxy --log -- ORIGINAL COMMAND` (its env and other settings kept). A Streamable HTTP server whose entry carries headers or a bearer-token variable is routed through `http://127.0.0.1:PORT/mcp` (from 8809, one port per upstream URL), where a LaunchAgent (`com.zazzle.proxy.PORT`, macOS) keeps `zazzled proxy --upstream URL` running; headers pass through. HTTP servers without headers sign in with OAuth, which the proxy cannot carry, and SSE servers use a transport it does not speak, so both are left alone with the reason in the report. `--no-wrap` leaves every other server alone.
- Without `--apply`, `--undo` or `--check` it changes nothing and prints the plan. Clients that look installed are chosen; `--client NAME` (repeatable) chooses instead. `--apply` backs up each file it changes (`FILE.zazzle-backup-SECONDS`), and a second run changes nothing. Quit the apps first: they rewrite their settings files. Routes are recorded in `~/.zazzle/setup.json`.
- `--undo` removes the zazzle entries, unwraps and unroutes the servers it changed, stops the proxy agents, and removes the instructions, the hooks and the workspace lines it added (lines you wrote stay).
- Servers setup does not change, and so stay direct, are listed with the reason: OAuth and SSE servers, servers a Claude Code or Codex plugin provides (a plugin update would overwrite the change), and servers in repositories' shared `.mcp.json` and `.codex/config.toml` under `--repo` and your workspaces. Servers added after setup show up in `--check` as `direct: not wrapped yet`; running `--apply` again wraps them. Wrapping changes only `command` and `args`: env, `env_vars`, cwd, enabled state, tool lists, timeouts and approval settings stay as they were.
- `--check` stays local by default: it reads the clients' settings, marks each server, tests the hook and zazzle itself, and starts or contacts no other server (`not started or contacted (--probe does)`). `--check --probe` starts every configured server the way its client does (same command, environment and folder: the entry's `cwd`, `/` for the desktop app, the project for Claude Code's project servers, else `--repo` or the current folder) and does a real handshake: `initialize`, `tools/list`, and for zazzle `zazzle_overview`; HTTP servers get an `initialize` with their headers (a 401 is reported as needing sign-in). Secrets are never printed, and an HTML error page is summarized by its size. Each server is marked `[zazzle]`, `[proxied: stdio]`, `[proxied: http]` or `[direct: why]`, and each client's routing is `enforced by the hook` (with a self-test that feeds the hook `rg`, `cargo test` and `git status`) or `instructions only`. It also says where zazzle may edit from the current folder, and sums up the last day of the activity log.
- `--verify` uses zazzle the way an agent would, on `--repo` or the current repository: `zazzle_find`, `zazzle_read source=true`, a reversible edit (it creates `.zazzle-verify-PID.txt`, edits it with the read's sha, and removes it) and `zazzle_overview`, once with zazzle started inside the repository and once started in your home folder with absolute paths (which only works when the repository is in a workspace); then `zazzle run` with a failing command (exit code kept), `tools/list` through every wrapped server, and each hook's self-test. It ends with the activity-log entries the run produced as evidence.
- `--report` reads the activity log (`~/.zazzle/activity.log`, `ZAZZLE_ACTIVITY_LOG` to move it): zazzle commands by name and session, `zazzle run` runs, MCP calls through the proxy by server and tool, raw commands the hook redirected, and bypasses: native edits, `# allow-raw` and calls let through while zazzle was unavailable. It also lists what the log cannot see.
- `~/.zazzle/policy.toml` holds your own choices: `instructions.extra` (added inside zazzle's marked block; text outside the markers is never touched) and the Claude Code session keys setup may own (`autoCompactWindow`, also per model under `modelSettings`, `promptSuggestionEnabled`, `idleCompaction`, `effortLevel`). `--apply` writes it once with everything commented out, so nothing is owned until you name a key; setup records each key's earlier value, and removing the key or `--undo` restores it. `--explain` describes the file; `--set KEY=VALUE --apply` edits it (an empty value removes the key); `--report` ends with a compaction section recommending an autoCompactWindow.
- Claude Code also gets a SessionStart hook that runs `zazzled setup --check --brief`: silent when everything matches the last `--apply`, one line naming what drifted otherwise. An upgraded zazzled re-applies the last `--apply` once when it starts.
- zazzled writes privacy-preserving final response sizes, estimated tokens, hashes and session-dedupe outcomes to `~/.zazzle/usage.jsonl`. `ZAZZLE_USAGE_LOG` moves it; an empty value disables it. No arguments or response text are stored. `zazzle-csharp-indexer report` combines it with the C# producer log.

- `--off` undoes everything the last `--apply` did (zazzle entries, wrapped and routed servers, proxy agents, instructions, hooks, workspace lines) but keeps a record of its options in `~/.zazzle/setup.json`; `--on` applies them again. `--undo` and `--uninstall` forget the record. `--uninstall` also stops zazzled's own LaunchAgent and prints how to remove the programs and `~/.zazzle`.

## Benchmarking with and without zazzle

1. With zazzle set up (`zazzle setup --apply --workspace ~/dev`), quit Claude Code, the Claude desktop app and Codex.
2. `zazzle setup --off`, then start the apps: they run as if zazzle was never installed (the binaries stay, unused).
3. Run the benchmark task and note each client's own token and time figures (from its usage or cost view).
4. Quit the apps, `zazzle setup --on`, start them again (trust the hook again in Codex if it asks), and run the same task from the same commit.
5. `zazzle setup --report` shows what that run sent through zazzle and what bypassed it, so a result can be read against how much zazzle was actually used.

Keep everything else equal: the same model and settings, the same repository state (a worktree pinned to one commit helps), and a fresh chat each time. Run each side more than once; single runs vary.

## Routing: what is enforced and what is asked

| | Claude Code (terminal, VS Code, desktop Code tab) | Codex (CLI and app; the IDE extension reads the same `~/.codex`, but OpenAI's docs do not say it runs hooks, so check it with `--report`) | Claude desktop chat |
|---|---|---|---|
| raw `grep`/`rg`/`git grep`, `find`, `cat`/`head`/`tail`/`sed -n` in the shell | hook refuses, names the zazzle tool | hook refuses, names the zazzle tool | no shell |
| builds and tests (`cargo`, `dotnet`, `go`, `npm`/`pnpm`/`yarn`, `make`, `pytest`, `gradle`, `mvn`, `tsc`, ...) | hook refuses, asks for `zazzle run -- CMD` | same | no shell |
| built-in search and listing | Grep and Glob refused | none besides the shell | none |
| built-in reads | Read of `.cs`, or of a file over 24 KB read whole, refused; other reads allowed | none besides the shell | none |
| edits | Edit/Write allowed and logged as native edits; refused with `--hook-edits` | `apply_patch` allowed and logged; refused with `--hook-edits` | zazzle_edit only |
| other MCP servers | wrapped stdio servers and routed HTTP servers pass through zazzle's proxy | same | same |
| everything else | instructions only | instructions only | instructions only |

- The hook (`zazzled hook`) answers in the PreToolUse format both clients read (`permissionDecision: deny` with the reason the agent sees). It never rewrites a command or approves one, so permission prompts and sandboxing stay as they were; the agent re-issues the command through zazzle, which goes through the same approvals.
- A command ending in `# allow-raw` passes and is logged as a bypass. When zazzle cannot be reached, the hook lets everything through and logs `zazzle-unavailable`; `ZAZZLE_HOOK=off` turns it off.
- Repository instructions win over zazzle's instructions when they conflict, but they rarely do: when a repository says "run `cargo test`", the hook asks for exactly that command behind `zazzle run --`, which runs it unchanged with the same exit code. If an instruction insists on a native tool, `# allow-raw` keeps it possible and visible.
- Not intercepted: Claude Code's Read of non-C# files (Edit needs it), WebFetch/WebSearch and hosted tools, MCP servers that stay direct, the desktop chat, Codex sessions that have not trusted the hook, and anything run outside an agent. `--report` lists these as not visible rather than counting them as covered.

## Commands

| command | output |
|---|---|
| `find PATTERN [PATH..] [-i -F -w -c -l -u] [-C N] [-g GLOB] [-m N]` | grouped by file, `N:` match and `N-` context lines, long lines clipped around the match, capped (50) with a footer saying how to narrow; ordinary files before deep files before tests |
| `read [REV:]PATH[:A-B].. [--symbol NAME[#N] --around N --ctx N --head N --tail N --budget T --anchors --full --terse]` | the literal source: header with line count and sha, then `N<TAB>text`; past the budget (default 6000 tokens, at most 16000, also for MCP and `--full`) it shows the selection's leading lines, marks the header INCOMPLETE and ends with the omitted range and the read that continues it; several paths share that budget evenly, and files past a spent budget are named instead of read; a missing path is answered with up to three similar files from the work tree (also for `outline` and `ls`); `--symbol` reads one symbol with its doc comments and attributes (see [Symbols](#symbols)); `--terse` gives the lossy terse views below instead (opt-in; `--source` is accepted and changes nothing) |
| `ls [DIR] [-d N] [-m N]` | tree with recursive file counts and line counts |
| `outline PATH.. [-d N] [-m N]` | `start-end kind name` per symbol; tree-sitter for Rust, Python, C, C#, JS/TS/TSX up to 1 MiB; line-based for larger files, Markdown, TOML, JSON and everything else, which in C#, Java, C++, Dart, Groovy, Kotlin and Scala also finds methods and constructors (test methods under `[Fact]` or `@Test` included) when no grammar is installed |
| `refs SYMBOL [PATH..] [--also SYMBOL]..` | definitions first, then uses with their enclosing symbol; in C# files a mention in a comment or string is not a use (tree-sitter) |
| `edit PATH ...` | `--old/--new` unique replace, `--lines` or `--after`, each with `--expect SHA` (from `read`) or `--at` anchors; `--create`; atomic, CRLF-preserving, confined to the working directory, its git work tree, the client's roots and your workspaces |
| `run [--lossless --budget T --timeout SECS --profile NAME\|none] [--] CMD ARGS..` | runs a command and prints its output cleaned with a profile, then `[exit N in Ts; …]`; exits with the command's code: see [Run](#run) |
| `proxy ...` | runs the MCP proxy as its own process: see [MCP proxy](#mcp-proxy) |
| `CMD \| clean [--as auto\|grep\|json\|text] [--lossless] [--budget T] [--stats]` | the piped output of any command, shrunk: see [Clean](#clean) |
| `overview`, `find-symbol`, `changed-since`, `git-log`, `git-blame`, `report-gap` | structure answered by `zazzle-csharp-indexer`: see [C# structure](#c-structure) |
| `status` | daemon, cache and session counters |
| `agent llm [--model NAME] [--effort low\|medium\|high]`, `agent prompt [--model NAME] [--effort E] [TEXT.. \| -]` | open models run locally: the catalogue, or download one and make it the default; ask one and stream its answer: see [Local models](#local-models) |
| `languages [jit [on\|off] \| fetch [NAME..]]` | the language modules: installed and missing ones, whether they download on first use, or download the missing ones: see [Language modules](#language-modules) |

Any command accepts `--budget T` (caps output at about T tokens, default 8000; `read` and `clean` have their own) and `--disable-session-deduping`.

`zazzle` alone (or `zazzle help`) prints an overview of the commands, grouped by purpose and wrapped to 80 columns; `zazzle help COMMAND` and `zazzle COMMAND --help` print a command's usage, options with their defaults, and examples (`src/cmd/help.rs`, one table for all of them). `run`, `proxy` and `setup` print the same pages from `zazzled`, which the invoker hands them to.

## C# terse view

`zazzle read --terse FILE.cs` returns a terse behavioural view from `zazzle-csharp-indexer` (Roslyn) instead of the code, and no sha. The default read (or `--anchors`) returns the code and its sha, which every edit needs.

- Hidden or collapsed: using blocks (`1-12 ⋯ 12 using(s)`), logging and tracing statements on loggers, `Debug`/`Trace` and spans with no side effects in their arguments, `if (logger.IsEnabled(..))` blocks that only log, `#region` lines, constructors that only assign fields (`13-18 ⋯ ctor(ILogger<X> logger, IRepo repo): only assigns fields`), and catch clauses that only log and `throw;`.
- Shortened: `public`/`private`/`protected`/`internal` on declarations, `var` where the type is spelled out on the right (`new T(..)`, `(T)x`, `x as T`), and local variables of 6+ characters, to the initials of their words (`invoiceRepository` → `ir`) or three letters of one word. Locals are bound by a compilation of the file, so fields, members and parameters with the same name keep theirs; a clash or keyword gets a number; a local used in `nameof` keeps its name. Parameters are never renamed.
- Kept: everything else, including catches that return or convert, constructors with logic, and logging that shares a line with other code. Lines keep their source numbers: `N` unchanged, `N~` changed, `A-B ⋯ note` hidden. Ranges, `--symbol` and the budget work on the view.
- The bridge: `zazzled` connects to the indexer daemon's socket (in `$ZAZZLE_INDEXER_STATE_DIR` or the indexer's default state directory), starting `zazzle-csharp-indexer daemon` when nothing listens, and asks its `view` tool. The indexer is `$ZAZZLE_INDEXER`, else beside `zazzled`, else on `PATH`. The file must be inside a git work tree. When there is no view (no indexer, not in git, an error), the source is returned with a line saying why. A daemon of another version gives way to a newer zazzled once its clients finish, and zazzled waits up to 15 s for it; after a failed connection zazzled answers `C# indexer unavailable: WHY` at once for 15 s instead of trying again for each call. The first read starts the daemon (about 0.5 s); later ones take a few milliseconds.

## HTML and Razor terse view

`zazzle read --terse` on `.html`, `.htm`, `.xhtml`, `.cshtml` and `.razor` files returns a terse view built inside `zazzled`, in the same line format and with no sha; the default read returns the code. A file with nothing to shorten comes back as source with its sha.

- Hidden: comments of 3+ lines (`<!-- -->`, Razor `@* *@`), `<style>` and `<svg>` elements that start and end their own lines, runs of 2+ `<meta>`/`<link>`/`<base>` lines, and in Razor runs of 2+ `@using`/`@addTagHelper`/`@removeTagHelper` lines.
- Runs of 10+ sibling elements with the same tag (`<li>`, `<tr>`, `<option>`), each closing on its line or at its own indentation, keep their first and last (`⋯ 38 <tr> element(s)`).
- Shortened: `class` values with more than 4 classes to their first 3 and `…`, `style` values over 40 characters to `…`; values containing `@`, `{{` or `${` are left alone.
- Razor `@code` and `@functions` blocks: the C# inside gets the C# terse view from `zazzle-csharp-indexer`, as a `.cs` file would (zazzled sends the blocks as text padded to the file's line numbers, so the view keeps them); without the indexer or a git work tree they are shown as they are.
- Razor `@{ }` blocks over several lines get the same C# view, wrapped as a method body: logging goes and long locals get short names. A local that the rest of the page also names (`@pageTitle` in the markup) keeps its name, because zazzled adds a `nameof(..)` of every word the block shares with the page, which the C# view never renames. A block with markup in it (`<p>`, `@:`) and a one-line `@{ .. }` stay as they are.
- Shown as they are: `<script>`, `<pre>` and `<textarea>` blocks. A comment or element that shares its first or last line with other markup is not hidden.

## CSS, SCSS, Sass and Less terse view

`zazzle read --terse` on `.css`, `.scss`, `.sass` and `.less` files returns a terse view built inside `zazzled` (`src/styles.rs`), line by line, in the same line format and with no sha; the default read returns the code. A file with nothing to shorten comes back as source with its sha.

- Hidden: `/* */` comments of 4+ lines and SCSS `//` runs of 4+ lines (shorter ones are usually section titles), `/*# sourceMappingURL */` comments, runs of 2+ `@import`/`@use`/`@forward` lines, `@font-face` blocks (`7-10 ⋯ @font-face "Inter", 4 line(s)`) and `@keyframes` blocks, vendor-prefixed declarations (`-webkit-`, `-moz-`, `-ms-`, `-o-`) when the same block also sets the plain property, and SCSS and Sass `@debug` lines. Less is read like SCSS; indented Sass finds its blocks (comments, `@font-face`, `@keyframes`, the block a prefixed declaration belongs to) by indentation and needs no `;`.
- Runs of 10+ sibling rules that set the same properties, each on one line or over several (icon fonts), keep their first and last (`⋯ 1418 rule(s) setting --fa`); so do runs of 10+ small rules of one or two declarations each that set different properties (utility classes), whose note names the properties up to about 80 characters (`⋯ 40 rule(s) setting margin-top, margin-bottom, …`). Sass is not checked for them.
- Runs of 12+ declarations in one block, one per line (a `:root` of custom properties), keep their first and last (`⋯ 115 declaration(s)`). Not in Sass.
- Shortened: `url(data:…)` values longer than 60 characters, and selector lines over 120 characters, cut to the selectors that fit in about 80 (`.a,.b,.c,… +17 more {`).
- Kept: everything else; a comment or block that shares its first or last line with other rules is not hidden. Minified CSS is reformatted first; see below.

## JSON, YAML, TOML and Markdown terse view

`zazzle read --terse` on `.json`, `.jsonc`, `.json5`, `.yaml`, `.yml`, `.toml`, `.md`, `.markdown` and `.mdx` files, and on `Cargo.lock`, `poetry.lock`, `uv.lock`, `pdm.lock`, `composer.lock` and `Pipfile.lock`, returns a terse view built line by line (`src/data.rs`, `src/md.rs`), in the same line format and with no sha; the default read returns the file. A file with nothing to shorten comes back as source with its sha.

- Lists: a run of 10+ sibling items in a JSON array, a YAML sequence or a TOML array keeps its first and last items and collapses the rest (`5-60 ⋯ 18 item(s)`). Items that span lines collapse only when they all name the same keys, so CI steps and other lists of different things stay. Runs nested in a hidden item go with it.
- Lock files (`*.lock`, `*-lock.json`, `*-lock.yaml`, `*.lock.json`): any 10+ sibling entries collapse the same way (`⋯ 365 entries`), so a `package-lock.json` reads as its first and last packages.
- TOML: a run of 6+ `[[name]]` tables with the same name keeps its first and last (`⋯ 310 [[package]] table(s)` in `Cargo.lock`).
- Hidden: `#` comment blocks (`//` in JSON with comments) of 4+ lines; YAML block scalars (`key: |`, `- >`) and TOML multi-line strings of 6+ lines keep their opening line (`⋯ text, 12 line(s)`). Quoted strings longer than 120 characters keep their first 40 (`N~`).
- Markdown: HTML comments (multi-line ones as `⋯ comment, N line(s)`), lines made only of badge images (shields.io, badgen, codecov, coveralls, `badge` or `.svg` sources), runs of 2+ link reference definitions (`[x]: url`) and lists of 3+ entries that only link to anchors (tables of contents). Fenced code and front matter are left alone.
- Kept: everything else. Minified JSON is reformatted first; see below.

## Minified JSON, XML, SVG, HTML and CSS terse view

A `.json`, XML or SVG, `.html`/`.htm`/`.xhtml` or `.css` file is minified when most of its text sits on lines of 500+ characters and its other lines are at most 10 (a licence banner, a source map comment) or fewer than the long ones, so a hand-written SVG with long path data is not. `src/minified.rs` reformats it in memory and runs the JSON, XML, HTML or CSS view over the result:

- JSON is reformatted with two spaces and one value per line; strings and numbers stay as written. XML and SVG get one element per line, with an element holding only text kept on one line.
- HTML gets the same, tolerating what browsers do: void elements (`<br>`, `<img>`, `<meta>`) stand alone, a stray or implied closing tag is not an error, and the text of `<script>`, `<style>` and `<textarea>` stays one line, cut to its first 80 characters when over 200 (`… (5120 characters)`).
- CSS gets one selector, declaration, closing brace or comment per line, indented a level per block; strings and parentheses (`url(..)`, `:not(..)`) stay whole.
- Every view line maps back to its source line: a kept line reads `1~`, and a note reads `1-1 ⋯ 38 item(s)`. The header counts the source's lines and adds `N minified lines reformatted`.
- Minified JSON is treated as generated data. Like a lock file, any 10+ sibling items or object members collapse, even when the items name different keys. A note on hidden members lists their keys up to about 80 characters (`⋯ 12 member(s): "api", "browsers", "css", …`).
- XML follows the usual rules: runs of 10+ same-tag elements keep their first and last, and long values are cut.
- If the text does not parse, or nothing was shortened, the file reads as it would have before. A view is kept even when it is longer than the minified file, since the token budget can cut a view at line boundaries but not inside one huge line.
- When the view of a one-line file is over the token budget, the footer says how many view lines were elided and to read with `--full`. A line range cannot narrow a one-line file.

## XML, INI and Dockerfile terse view

`zazzle read --terse` on XML files (`.xml`, `.csproj`, `.fsproj`, `.vbproj`, `.vcxproj`, `.props`, `.targets`, `.xaml`, `.axaml`, `.plist`, `.resx`, `.xsd`, `.xsl`, `.xslt`, `.wsdl`, `.nuspec`, `.manifest`, `.svg`), INI-style files (`.ini`, `.cfg`, `.properties`, `.editorconfig`) and Dockerfiles (`Dockerfile`, `Dockerfile.*`, `*.dockerfile`, `Containerfile`) returns a terse view built line by line (`src/xml.rs`, `src/config.rs`), with no sha; the default read returns the file.

- XML: comments of 3+ lines (`⋯ comment, N line(s)`); runs of 10+ sibling elements with the same tag, one per line or each closing at the same indentation, keep their first and last (`⋯ 40 <PackageReference> element(s)`); quoted values over 120 characters and runs of 120+ base64-style characters keep their first 40 (`N~`).
- SVG: as XML, with long path data cut the same way, and editor metadata hidden whatever its shape: `<metadata>` and any `sodipodi:` or `inkscape:` element (`⋯ <sodipodi:namedview>, 12 line(s)`).
- INI: `;` and `#` comment blocks of 4+ lines, and long values as in XML.
- Dockerfile: `#` comment blocks of 4+ lines (`# syntax=` and `# escape=` directives stay), runs of 3+ `LABEL` lines (or one `LABEL` continued over 3+ lines), and 6+ package names one per continued line after a `RUN … \` line (`⋯ 25 package(s)`).
- Kept: everything else, including short comments, mixed tags and commands between packages.

## HCL/Terraform, Protobuf and GraphQL terse view

`zazzle read --terse` on HCL files (`.tf`, `.tfvars`, `.hcl`, including `.terraform.lock.hcl`), Protobuf files (`.proto`) and GraphQL files (`.graphql`, `.graphqls`, `.gql`) returns a terse view built line by line (`src/schema.rs`), with no sha; the default read returns the file. These are declarations, so the view needs no grammar module.

- Comments: runs of 3+ comment lines (`#` and `//` in HCL, `//` in Protobuf, `#` in GraphQL) and `/* */` blocks of 3+ lines (`⋯ comment, N line(s)`).
- GraphQL `"""` descriptions over several lines become one note quoting their first line of text, cut to 100 characters (`5-7 ⋯ """The ID of the migration."""`, or `… """ (4 lines of text)` when there is more); one-line descriptions stay.
- HCL heredoc bodies of 3+ lines (`<<EOT`, `<<-EOT`): the opening line stays, the body and terminator become `⋯ heredoc, N line(s)`.
- Protobuf runs of 3+ `import` lines and of 3+ top-level one-line `option` lines (`⋯ 6 option(s)`).
- Runs of 10+ one-line items keep their first and last: the values of a Protobuf or GraphQL `enum` (`⋯ 10 value(s)`; GraphQL descriptions between values belong to the run), and the items of an HCL list (`= [`) or map (`= {`), such as tags, CIDR blocks and lock-file `hashes` (`⋯ 11 item(s)`). Comments and blank lines may sit between items; a multi-line item ends the run.
- Quoted values over 120 characters and runs of 120+ base64-style characters keep their first 40 (`N~`).
- Kept: everything else, including resource and block attributes, message fields, types and their fields, short comments and services.

## CMake and Makefile terse view

`zazzle read --terse` on CMake files (`CMakeLists.txt`, `.cmake`) and Makefiles (`Makefile`, `makefile`, `GNUmakefile`, `Makefile.*`, `.mk`, `.mak`) returns a terse view built line by line (`src/buildfile.rs`), with no sha; the default read returns the file.

- Comments: runs of 3+ `#` lines and CMake bracket comments (`#[[ ]]`, `#[==[ ]==]`) of 3+ lines (`⋯ comment, N line(s)`).
- Runs of 3+ includes (CMake `include(..)`, Make `include`, `-include`, `sinclude`) become `⋯ 3 include(s)`.
- Lists of 6+ one-word items keep their first and last item (`⋯ 49 item(s)`):
  - in CMake, the arguments of a command left open on its line (`set(SOURCES`, `add_library(core`, `target_link_libraries(..`), stopping at a keyword such as `PRIVATE` or `PUBLIC`;
  - in Make, the continued lines after a line ending in `\` (`SRCS := \`, kernel `obj-y += \`).
- Arguments of `function`, `macro`, `add_custom_command`, `add_custom_target`, `execute_process` and `add_test` stay whole, because they are signatures and command lines.
- Logging is hidden: CMake `message(STATUS|VERBOSE|DEBUG|TRACE|CHECK_START|CHECK_PASS|CHECK_FAIL ..)`, even over several lines, and a Make `$(info ..)` alone on its line. `message(WARNING ..)`, `FATAL_ERROR`, `SEND_ERROR`, `AUTHOR_WARNING`, plain `message(..)` and `$(warning ..)` stay.
- Quoted values over 120 characters and runs of 120+ base64-style characters keep their first 40 (`N~`).
- Kept: everything else, including targets, recipes, conditions and short comments.

## Python, JavaScript, TypeScript, Dart, Rust, Go, Java, Kotlin, Swift, C, C++, Ruby, PHP, Scala, Perl, Lua, Elixir, Haskell, OCaml, F#, Clojure, Erlang, R, Julia, Zig, Objective-C, Groovy, PowerShell, SQL, Visual Basic, Fortran, Pascal, COBOL, Nim and shell terse view

`zazzle read --terse` on `.py`, `.pyi`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.ts`, `.mts`, `.cts`, `.tsx`, `.dart`, `.rs`, `.go`, `.java`, `.kt`, `.kts`, `.swift`, `.c`, `.h`, `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx`, `.rb`, `.rake`, `.php`, `.scala`, `.sc`, `.pl`, `.pm`, `.lua`, `.ex`, `.exs`, `.hs`, `.ml`, `.fs`, `.fsx`, `.clj`, `.cljs`, `.cljc`, `.erl`, `.hrl`, `.r`, `.jl`, `.zig`, `.m`, `.groovy`, `.gvy`, `.gradle`, `.ps1`, `.psm1`, `.vb`, `.f`, `.for`, `.ftn`, `.f90`, `.f95`, `.f03`, `.f08`, `.fpp`, `.pas`, `.dpr`, `.lpr`, `.dpk`, `.sh`, `.bash`, `.zsh` and `.ksh` files (`.h` is read as C++) returns a terse view built inside `zazzled` with tree-sitter (`src/terse.rs`), in the same line format and with no sha; the default read returns the code. A file with nothing to shorten, or over 1 MiB, comes back as source with its sha.

- Hidden or collapsed: runs of 2+ lines of top-level imports (`1-6 ⋯ 3 import(s)`; Rust `use`, Go `import (..)`, Java and Kotlin `import`, Swift and Scala `import`, C and C++ `#include`, PHP `use`, Ruby `require`/`require_relative`, Perl `use`, Lua `local x = require ..`, Elixir `alias`/`import`/`require`/`use` at the top or inside a `defmodule`, Haskell `import`, OCaml and F# `open`, Clojure `(require ..)`/`(import ..)`/`(use ..)` and the `(:require ..)` clauses of `ns`, Erlang `-include`/`-include_lib`/`-import`, R `library`/`require`/`source`, Julia `using`/`import`, Zig `const x = @import(..)`, Objective-C `#import`/`@import`, Groovy `import`, PowerShell `Import-Module`/`using`/`. file`, Visual Basic `Imports`, Fortran `use`/`include` (in each program unit), a multi-line Pascal `uses` clause (`⋯ 12 import(s)`), shell `source`/`.`); logging statements on loggers (`log`, `logger`, `_logger`, `self.logger`, `this.logger?`, `LOG`, `slog`, ...), JavaScript and TypeScript `console.log/debug/info/warn/error/trace`, Dart `debugPrint`/`log`, Rust `debug!`/`info!`/`warn!`/`error!`/`trace!`/`event!` (bare or under `log::`/`tracing::`), Android `Log.d/i/w/e/v` and Timber, Swift `NSLog`/`os_log`/`debugPrint`, C and C++ `spdlog::`/`logger->` calls, `LOG_*`/`DLOG*`/`ALOG*`/`VLOG*`/`LOGE`-style macros `LOG(INFO) << ..`/`qDebug() << ..` streams, Ruby `logger.*`/`Rails.logger.*` and PHP `$this->logger->*`, `Log::*` and `error_log`, Perl `$log->debug(..)`-style calls and Log4perl's bare `DEBUG`/`INFO`/`WARN`/`ERROR`/`TRACE`, Lua `log.*`/`logger:*`, Elixir `Logger.*`, Haskell `logDebug`/`logInfo`/`$(logWarn)` (monad-logger) and `debugM`/`infoM` (hslogger), OCaml `Logs.*`/`Log.*`, Clojure `log/*` and `timbre/*`, Erlang `?LOG_*` macros and `logger:*`/`lager:*`, R `log_*` (logger) and `flog.*` (futile.logger), Julia `@debug`/`@info`/`@warn`/`@error`/`@logmsg`, Zig `std.log.*`/`log.*` (`std.debug.print` stays), Objective-C `NSLog`/`os_log`/`DDLog*`, PowerShell `Write-Verbose`/`Write-Debug`/`Write-Information`/`Write-Log` without `$(..)` or a pipe (`Write-Host` stays), Visual Basic `Debug.*`/`Trace.*`, Fortran `call log_*(..)` and `call logger%info(..)`, Pascal `Log.d(..)`/`Logger.Info(..)`/`CodeSite.Send*(..)`/`OutputDebugString(..)` (`WriteLn` stays), shell commands named `log`, `log_*`, `debug`, `debug_*` or `logger` with no `$(..)`, process substitution or arithmetic in their arguments (`echo` and `printf` stay, they are the script's output), when they are alone on their lines and have no assignment, await, yield, increment, decrement, Go channel receive or Rust `?` in their arguments (Go `log.Fatal*` and `log.Panic*` stay: they stop the program); `if` blocks without `else` that check a log level (`isDebugEnabled`, `isEnabledFor`, ...) and only log; region markers (`# region`, `// #region`, `//#endregion`, `--#region`, `;; region`, `%% region`, `(* region *)`, Visual Basic `#Region`, Fortran `! region`, Pascal `{$REGION}`/`{$ENDREGION}` and `{%region}`, `#pragma region`; Swift `// MARK:` comments stay, they are section titles); multi-line constructors that only store parameters (`__init__` with `self.x = name`, docstrings, `pass` and `super().__init__(..)`; TypeScript `this.x = name` and `super(..)`; Dart constructors without a body whose initializers are `field = name` or `super(..)`; Java constructors with `this.x = name` and `super(..)`/`this(..)`; Rust functions whose body is only `Self { a, b: name }`; Go functions that only `return &T{a: name}`; Swift `init` with only `self.x = name`; C++ constructors with an empty body and an initializer list of `field_(name)`; Ruby `initialize` with only `@x = name`; PHP `__construct` with only `$this->x = $name` or promoted parameters; Groovy as Java; Visual Basic `Sub New` with only `_x = name` or `Me.x = name`; a Pascal `constructor` with only `inherited` and `FX := AX`) as `12-17 ⋯ __init__(self, api, cache): only assigns fields`; and catches that only log and rethrow (bare `raise`, `throw e` of the caught name, `rethrow`, C++ `throw;`, Swift `throw error` or the bound name, Ruby bare `raise` in a `rescue`, PHP `throw $e`, a Scala catch with one `case e: T =>` that throws `e`, Groovy `throw e`, Objective-C `@throw;`, PowerShell `throw`, Visual Basic `Throw`, a Pascal `on E: X do begin .. raise; end` handler), whole when the clause has its own lines, else as `} catch (e) { ⋯ logs and rethrows }`; Go's `if err != nil {` blocks that only log and return the error with plain values (`return nil, err`) stay on one line as `if err != nil { return nil, err }` (`{ ⋯ logs; return nil, err }` when they logged).
- Shortened: `public`/`private`/`protected` on TypeScript class members (constructor parameter properties keep theirs) and on Java declarations; Kotlin and Swift visibility modifiers; PHP `public`/`private`/`protected` on methods, properties and constants (promoted constructor parameters keep theirs); Scala `private`/`protected`; Groovy as Java; Visual Basic `Public`/`Private`/`Protected`/`Friend`; Rust `pub`/`pub(crate)` except on `pub use`.
- Shell heredoc bodies of 6+ lines collapse to one note; the `<<EOF` and `EOF` lines stay. SQL files are read line by line (`src/sql.rs`): `-- region` and `/* region */` comments go, and an `INSERT .. VALUES` whose 6+ rows each sit alone on their lines keeps its first and last rows and collapses the rest (`⋯ 5 row(s)`).
- In Haskell, OCaml, F#, Clojure, Erlang, R and Julia a block's last expression is its value, so a log call there stays; Clojure log calls are hidden only in body forms (`defn`, `let`, `do`, `when`, ...), not as arguments.
- Short locals (in the languages listed in the heading up to Elixir, in Zig, Objective-C and Groovy, and by plainer rules in Erlang, Visual Basic, Pascal and free-form Fortran; Groovy only in methods without closures, lambdas or command calls, which its grammar misreads; not in Haskell, OCaml, F#, Clojure, R, Julia, PowerShell, SQL, COBOL or Nim, whose pattern bindings, shadowing or dynamic names make renames unsafe without a compiler; not in shell scripts, where `local` variables are dynamically scoped and reached by name through `read`, `unset`, `printf -v`, `${!x}` and `declare -n`): locals of 6+ characters get the C# view's short names (`invoice_rows` → `ir`, `runningTotal` → `rt`, `response` → `res`; a clash, keyword or common builtin gets a number) at their declaration and every use (`src/locals.rs`). Without a compiler, a name is renamed at every place it appears in its function, and only when that is provably safe. It keeps its name when it is also a parameter, a nested function or class name, a JSX tag, an object or struct shorthand key, a Go struct-literal key, a Java label or annotation, in a Java local or anonymous class, in `global`/`nonlocal`, an import or a Rust `use`, a Rust macro name or a Rust format string (`{name}`), a Kotlin `$name` template, a C++ init-capture (`[x = y]`), a Ruby method called without a receiver or used before its first assignment, a PHP static property (`self::$x`), `global`/`static` declaration or variable variable (`$$x`), a Scala lambda parameter, a Perl sub's `@_`/`shift` parameters or a name written with another sigil (`$x` vs `$x[0]`, which is `@x`), an Elixir function-head argument or a name bound in a `case`/`fn` pattern, a Lua or Perl name used inside its own declaration (`local x = x + 1`, where it means the outer `x`) or, as in Ruby, before its first declaration, inside a preprocessor line or named in a `#define` of the same file, also written as a type name (C++'s vexing parse), inside a `match` case or a self-documenting f-string field (`{x=}`); outside Python, Ruby, PHP and Elixir (where a local belongs to the whole function) also when it is declared twice (Rust shadowing, Go `:=` redeclaration) or used outside the block that declares it; and in a function that uses `eval`, `locals()`, `vars()`, `exec`, `with`, Ruby `binding`/`local_variable_get`, PHP `compact`/`extract`/`get_defined_vars`, Perl `no strict` or a string `eval`, Lua `load`/`loadstring`/`setfenv`/`getfenv` or `debug.getlocal`/`setlocal` calls, or Elixir `quote`/`binding`/`var!`, whose parse has errors, or that a macro made the parser misread (a C "function" without a function declarator). Members (`obj.name`) and keyword or named-argument labels are other things and keep their text.
- Plainer short locals (`src/locals.rs`, `plain_renames`): in an Erlang function clause, a Visual Basic procedure or accessor, a Pascal routine with its nested routines, or a Fortran program unit with its internal procedures, a declared local gets a short name at every occurrence of its name in the routine. A local is an Erlang variable not starting with `_`, a Visual Basic `Dim` or typed `For` variable, a Pascal `var` name, or a Fortran declared name that is not a dummy argument or result. Visual Basic, Pascal and Fortran match names in any case, and Erlang short names are capitalized (`RunningTotal` → `Rt`). A name keeps its spelling when any occurrence is a member, field, named or keyword argument, parameter, routine or macro name; when a `-define` in the file names it (Erlang macros can read the caller's variables); when the routine has a Pascal `with`, an include, or a Fortran `namelist` or `interface`; when its count in the routine's text (outside comments and strings) differs from its identifiers, which catches grammar misreads such as Visual Basic's `NewDate` read as `New Date`; and, in Visual Basic, when it is declared twice or used outside its block. Fixed-form Fortran (`.f`, `.for`) mostly parses with errors, so it keeps its names.
- Kept: everything else, including `print` and logging that shares a line with other code.
- COBOL (`.cbl`, `.cob`, `.cpy`, `.cobol`) has no maintained tree-sitter grammar, so `src/cobol.rs` works line by line: in fixed format it drops the sequence numbers (columns 1-6) and identification area (73-80), marking those lines `N~`, hides debugging lines (`D` in column 7) and collapses comment blocks of 3+ lines (`*` or `/` in column 7, or `*>`) and runs of `COPY` statements (`⋯ 4 copybook(s)`).
- Nim (`.nim`, `.nims`) has no tree-sitter grammar good enough, so `src/nim.rs` works line by line: runs of top-level `import`/`from .. import`/`include` lines (following brackets and trailing commas), indented one-line `debug`/`info`/`notice`/`warn`/`error`/`fatal`/`trace` calls (bare or on `log`/`logger`) with no `=`, `inc`, `dec`, `add`, `del` or `discard` and no continuation line, and `# region` comments; lines in `"""` strings are left alone.

## Language modules

`zazzled` links no tree-sitter grammar. All 34 (Bash, C, C++, C#, Clojure, Dart, Elixir, Erlang, F#, Fortran, Go, Groovy, Haskell, Java, JavaScript, Julia, Kotlin, Lua, Objective-C, OCaml, Pascal, Perl, PHP, PowerShell, Python, R, Ruby, Rust, Scala, Swift, TSX, TypeScript, Visual Basic, Zig) are language modules: shared libraries built from `macos/rust/zazzle-lang` and published with each release under `https://icelander.net/downloads/zazzle/vX.Y.Z/languages/PLATFORM/` (`macos-universal`, `linux-x86_64`, `linux-aarch64`). A machine only downloads the grammars of the languages it reads, and `zazzled` is 6.8 MB (82.4 MB with every grammar linked in). Outlines and `read --symbol` use the Rust, Python, C, C#, JavaScript, TypeScript and TSX modules; until one is installed (JIT off, or offline), those files get the line-based outline.

- **Trust:** the release compiles each module's SHA-256 into `zazzled` (the manifest). A module is downloaded, stored and loaded only if its SHA-256 matches, so a module from another release, a changed file on the server or a corrupted local copy is never loaded (a bad local copy is downloaded again). On macOS the release signs the modules with the same identity as `zazzled` when signing is configured.
- **JIT (on by default):** the first time a file needs a missing module, `zazzled` downloads it (that read waits), checks it, keeps it in `~/.zazzle/v1/languages` and loads it with `dlopen`. With JIT off, or offline, such files read as source; a failed load is retried after a minute.
- `zazzle languages` lists the installed and missing modules, the JIT setting, the directory and the download URL.
- `zazzle languages jit on|off` turns downloading on first use on or off (stored in the directory's `jit` file).
- `zazzle languages fetch [NAME..]` downloads every missing module (or the named ones), for example before going offline.
- `ZAZZLE_LANGUAGES_DIR` and `ZAZZLE_LANGUAGES_URL` override the directory and the download URL; downloads are still checked against the manifest.
- Development builds carry no manifest and so no grammars (line-based outlines, no tree-sitter terse views). To try modules locally: `bash ../release/language-modules.sh /tmp/langs && bash ../release/language-modules.sh --manifest /tmp/langs`, build with `ZAZZLED_LANGUAGES_MANIFEST=/tmp/langs/manifest cargo build --release -p zazzled`, and serve `/tmp/langs` as `ZAZZLE_LANGUAGES_URL` (or copy the files into the languages directory). Tests link the module grammars statically.

## Local models

`zazzle agent llm` and `zazzle agent prompt` run open-weight models (Qwen, Gemma, gpt-oss) on the machine with llama.cpp: `zazzle agent llm --model qwen3.5-9b --effort high` downloads a model and makes it the default, `git diff | zazzle agent prompt "Review this diff:" -` asks it. The program behind them, `zazzle-llm` (`macos/rust/zazzle-llm`, where the catalogue, efforts and exit codes are described), is a JIT tool: the invoker execs `zazzled llm ARGS`, which uses a `zazzle-llm` beside zazzled or else the one in `~/.zazzle/v1/tools`, downloading it first from `https://icelander.net/downloads/zazzle/vX.Y.Z/tools/PLATFORM/` and accepting it only when its SHA-256 matches the manifest compiled into zazzled (`ZAZZLED_TOOLS_MANIFEST`; `src/tools.rs`). `ZAZZLE_TOOLS_DIR` and `ZAZZLE_TOOLS_URL` override the directory and URL. `zazzle-llm` in turn downloads each model from Hugging Face on first use, checked against the SHA-256 its catalogue pins. Through the daemon (MCP, `zazzled run`) these commands answer that they run as their own process.

## C# structure

`zazzle-csharp-indexer` lists no tools of its own: agents ask zazzle, and `zazzled` calls the indexer's daemon over the same bridge as the terse view (starting it when nothing listens), for the git work tree that holds the working directory. Answers come from the index the daemon keeps current, so they need no file reads. `root:path` in the indexer's answers becomes a path relative to the working directory.

A session opened in a folder above several repositories works too. When the working directory is not inside a work tree, zazzled looks for repositories below it: up to three levels deep, outermost only, skipping hidden folders, `node_modules`, `bin`, `obj` and `target`. `overview` then lists them, and `find-symbol` and `changed-since` search the first 24 together in one indexer call, with paths relative to the folder (`api/src/Order.cs:12`). `--repo DIR` (MCP `repo`) narrows any of these to the repository holding DIR. `git-log PATH` and `git-blame PATH` use the repository holding PATH, wherever the session is; `git-log` without a path asks for one in a folder of several repositories.

| MCP tool | command | output |
|---|---|---|
| `zazzle_overview` | `overview [--repo DIR]` | file and line counts by language, top directories, entry points and project files, largest files; start here on an unfamiliar repository |
| `zazzle_find_symbol` | `find-symbol NAME.. [--path DIR|GLOB].. [--exact] [--kind K] [--limit N] [--offset N] [--repo DIR]` | `path:line kind container.name \| signature`, exact matches first; C# parsed with Roslyn, other languages by a regex extractor |
| `zazzle_changed_since` | `changed-since [CURSOR] [--repo DIR]` | `A`/`M`/`D path` since a cursor from an earlier call, then a new cursor; cursors expire when the indexer's daemon restarts |
| `zazzle_git_log` | `git-log [PATH] [-n N] [--grep TEXT] [--repo DIR]` | `hash date author subject` |
| `zazzle_git_blame` | `git-blame PATH [--start N] [--end N]` | `git blame -w` for a range (default 40 lines) |
| `zazzle_report_gap` | `report-gap TEXT [--repo DIR]` | records what the agent needed and could not get in zazzle's activity log and, when the indexer runs, its usage log, where `zazzle-csharp-indexer report` lists it |

Where a symbol is used is `zazzle_refs` (`refs`); the indexer's `find_references` is gone. Unlike it, `refs` shows each use's enclosing symbol, but it lists declarations of fields, locals and parameters as uses rather than definitions, and pages with `-m` and narrower paths instead of an offset.

## Edits need the sha

Every edit except `--create` needs proof that the caller read the current code: `--expect SHA` from `read --source`, or `--at` anchors from `read --source --anchors`. A refusal never reveals the current sha or anchors (`the file is not the version you read; read it again with --source`). A successful edit returns the new sha, since the caller wrote the change.

## Pinning to a commit

Every repository command takes `--commit SHA` (MCP `commit`): a full sha, a prefix of at least four hex digits, or `HEAD`. zazzle works out which repositories the request touches (the paths it names, else the working directory's repository, else the repositories below the working directory) and answers only from those checked out at that commit:

- A path in a repository at another commit is refused, naming the commit it is at; if a worktree of that repository has the commit checked out, the refusal names it, and otherwise says whether the repository has the commit at all. zazzle never reads history: it answers from checked-out files.
- In a folder of several repositories (or worktrees), a search, listing or outline is narrowed to the repositories at the commit, so a sibling checked out elsewhere never contributes; the structure tools need one such repository (else `--repo`).
- The answer ends with `[commit SHA REPO]` for each repository it came from, with the full sha, and `, with uncommitted changes in these files` (or `in the work tree`) when the files read, or the tree searched, differ from the commit. `HEAD` asks for these lines without pinning.

## Paths

Paths are absolute or relative to the working directory. A relative path that is not there is also looked for from the folders above it that hold a solution (`.sln`, `.slnx`), up to the repository root, then from the repository root; in a folder of repositories, from the one repository below that has it. `REPO:path` names a repository the way zazzle shows it (`api:src/Orders/OrderService.cs`). New files (`edit --create`) still go where the path says from the working directory.

## Symbols

`zazzle read FILE --symbol NAME` (`-s`) reads one symbol instead of the whole file: the outline gives its range, which is extended upward over the doc comments, comments and attributes directly above it (`///`, `/** */`, `#[...]`, C# `[...]`, `@decorator`, Python `#`), so a C# method's XML doc comes with it.

- `NAME` is the symbol's name or the last parts of its qualified name: `Invoice.Total`, `billing::tax::total`, `Acme.Shop.Orders.OrderService.Submit`. Containers come from nesting (classes, impls, modules, namespaces, and a C# file-scoped `namespace X;` for everything after it); `impl Display for A` counts as `A`; generics are ignored. Exact case is tried first, then any case.
- Without a file (`zazzle read --symbol Acme.Shop.Orders.OrderService.Submit`, or `zazzle_read` with `symbol` and no `paths`), the file comes from zazzle-csharp-indexer's index: the one file that declares it is read; if several do, they are listed (`path:line kind Namespace.Type.Member | signature`) to pick from or to qualify the name further. `find-symbol Namespace.Type.Member` (or any trailing part) searches the index the same way.
- Overloads are picked by parameter types: `Total(decimal, int)`, `Total()`. Types are compared as written in the declaration, ignoring spaces (`Dictionary<string,int>` matches `Dictionary<string, int>`, but `decimal` does not match `System.Decimal`); both `Type name` and `name: Type` declarations work, defaults, attributes and modifiers (`ref`, `out`, `params`, `mut`) are ignored, receivers (`self`, `&self`, Python's `self`/`cls`) are not counted, and an untyped parameter matches by name. A list that fits no overload shows the overloads there are.
- Several matches (overloads, the same method in two impls) are listed with their lines and declaration line, numbered; add the parameter types or `#N` (`Total#2`) to pick one. No match lists similar names, or points at `zazzle outline`.
- The usual budget applies, and `--symbol` cannot be combined with `PATH:A-B`, `--around`, `--head` or `--tail`. Over MCP, `zazzle_read` takes `symbol`.

## Clean

`grep -rn foo src | zazzle clean` (or `cargo test 2>&1 | zazzle clean`) reads standard input, up to 64 MiB, and prints what an agent needs from it:

| step | what it does | `--lossless` |
|---|---|---|
| ansi | removes colours, cursor moves and window titles | yes |
| whitespace | keeps the last frame of `\r` redraws, trims line ends, squeezes blank runs | yes |
| json | a JSON document or JSON Lines loses whitespace outside strings; keys and numbers stay as written | yes |
| grep | `path:N:text` / `path-N-text` / `path:text` output (grep, git grep, rg) is regrouped by file in `find`'s layout, only if every line parses and it is shorter | yes |
| repeats | runs of identical lines become one line and `… previous line repeated N more times` | yes |
| progress | a run of progress-bar lines becomes its last line and `… N progress lines dropped` | no |
| blobs | base64 or hex runs of 200+ characters become `<blob N chars>` | no |
| long-lines | lines over 600 bytes keep 300 bytes and `[line was N bytes]` (not for JSON) | no |
| budget | above `--budget` (default 8000 tokens) the head and tail stay and the middle becomes one line saying how much was cut | always |

Every removal leaves a marker. Input that is not recognised only gets the lossless steps, and if the markers would make the text longer, the input is passed through. `--as` skips detection (`text` turns off the json and grep steps). `--stats` adds `[clean ~A→~B tokens; steps: …]`. Repeating the same cleaned output in a session gets the usual `already sent` notice. `clean` is not an MCP tool: an MCP client already holds the text. To clean what another MCP server returns, use the [MCP proxy](#mcp-proxy).

## Run

`zazzle run -- cargo test` runs a command in the caller's environment and prints what an agent needs from its output. The invoker `exec`s `zazzled wrap ARGS` (on Intel Macs run `zazzled wrap` directly); it never runs inside the daemon ([why](src/.dev/decisions.json)).

- stdout and stderr go through one pipe, so their order is kept; stdin is the caller's. The exit status is the command's (128 + N for a signal); `--timeout SECS` (default 3600) kills it and exits 124. Up to 64 MiB of output is read.
- A profile, picked from the command (through `npx`, `uv run`, `python -m`, `env VAR=x`, …) or named with `--profile`, drops only lines it knows are noise and counts them. Lines it does not recognise are always kept.

| profile | commands | drops |
|---|---|---|
| cargo | `cargo …`, `cargo-nextest` | progress (`Compiling`, `Running`, `Downloaded`, …), `test … ok`, nextest `PASS`, empty test runs, std/core frames in backtraces |
| pytest | `pytest`, `python -m pytest` | session header, `PASSED` lines, progress lines with no failure |
| go | `go test`, `go vet`, `go build` | `ok` packages, `--- PASS`, `=== RUN`, packages without tests |
| dotnet | `dotnet …` | restore and `->` build lines, banners, `Passed` tests, System/Microsoft/xUnit/NUnit frames, the repeated warnings and errors MSBuild prints again at the end |
| js-test | `jest`, `vitest`, `mocha`, `npm test` and the like | `PASS` files, `✓` tests, banners |
| tsc | `tsc`, `vue-tsc` (also through `npx`) | `tsc -b --verbose` progress and the `--pretty` per-file summary table; diagnostics are grouped under their file (`src/a.ts` then `1:14 error TS2322: …`), code frames kept |
| install | `npm/pnpm/yarn/bun install`, `pip install`, `uv pip install` | download and resolve progress |

- Then `clean`'s steps and budget apply (`--lossless` turns off the profile and the lossy steps), and a footer says what happened: `[exit 101 in 8.4s; cargo profile dropped 6 progress lines, 333 passing tests]`.
- Measured: this crate's `cargo test` (334 passing tests) goes from about 5,300 tokens to about 80; 40 `tsc` errors across 4 files go from about 1,300 tokens to 850 (2,800 to 1,500 with `--pretty`).

## MCP proxy

`zazzle proxy` sits between an MCP client and another MCP server and runs `clean`'s steps over the text the server sends back:

```bash
claude mcp add fs -- zazzle proxy -- npx -y @modelcontextprotocol/server-filesystem ~/src      # stdio server
zazzle proxy --upstream https://mcp.example.com/mcp --listen 127.0.0.1:8808                      # Streamable HTTP server
claude mcp add --transport http example http://127.0.0.1:8808/mcp
```

- `zazzle proxy ARGS` replaces itself (`exec`) with `zazzled proxy ARGS` and does not run inside the daemon ([why](src/.dev/decisions.json)). On Intel Macs, where there is no `zazzle`, run `zazzled proxy` directly.
- What changes: the `text` of `content` items (and of embedded `resource` items) in `tools/call` results, and the `text` of `contents` in `resources/read` results. A text is replaced only when that saves at least one token.
- What never changes: `structuredContent`, images and other binary items, errors, protocol messages (`initialize`, lists, notifications), requests in either direction, and replies to anything else. Responses are matched to requests by JSON-RPC id, single or batched.
- Options: `--lossless` and `--budget T` as for `clean` (budget per text, default 8000); `--skip TOOL` (repeatable) leaves one tool's results alone; `--log` writes `tools/call NAME ~A→~B tokens` to standard error for each cleaned result; `--config FILE` sets them per tool.
- `--config FILE` is JSON. Each tool (by exact name, else the longest matching `*` pattern) and resource reads can set `lossless`, `budget` and `skip`; what a rule leaves out comes from the command line. Unknown sections, settings or types stop the proxy at startup.

  ```json
  {"tools": {"read_file": {"lossless": true, "budget": 30000}, "github_*": {"budget": 2000}, "get_secret": {"skip": true}},
   "resources": {"lossless": true}}
  ```
- Tracing: `zazzle trace on [DIR]` (default `~/.zazzle/traces`) makes every proxy, already running or not, write each request it serves to a folder `DIR/MMDD_HHmmss` (`_2`, `_3` for more in the same second): `1-request.json` (the agent's JSON-RPC request), `2-response-raw.json` (the server's response as it came), `3-response.json` (what the proxy returned), and `4-results.json`: method and tool, server, request and response times, estimated tokens of the raw and returned responses with the saving and the request's own tokens, bytes, the text items' tokens, lines and count, the time spent upstream and in the filter, the filter's settings for that call (skipped, lossless, budget) and whether it changed anything, the read-again id of a stored original, whether the response was an error, and the proxy's pid and version. `zazzle trace off` stops it and `zazzle trace` says how it stands; the switch is the file `~/.zazzle/trace.json` (`$ZAZZLE_TRACE_CONTROL`), read on each request, so no proxy needs a restart. Notifications and server-to-client requests are not traced; at most 1000 requests per proxy wait for their responses.
- stdio: the server runs as a child with stderr passed through; the proxy exits with its exit code when it closes its output.
- HTTP: only Streamable HTTP (MCP 2025-03-26 and later), not the older HTTP+SSE transport. Connections stay open for further requests unless the client asks to close them (`Connection: close`, or HTTP/1.0 without keep-alive), sends something unreadable or stays idle for 60 seconds; replies of unknown length are chunked on open connections, and the upstream side reuses its connections too. JSON replies are cleaned whole (up to 64 MiB, larger ones are passed through), `text/event-stream` replies event by event as they stream, and anything else (including compressed bodies) is copied. Headers such as `Mcp-Session-Id` and `Authorization` pass both ways; hop-by-hop headers are dropped, bodies are requested uncompressed, redirects and error statuses are returned to the client. `Origin` headers from hosts other than localhost are refused (403). It listens on `127.0.0.1:8808` unless told otherwise; request bodies need `Content-Length`. Upstream TLS uses the operating system's trust store, and `HTTPS_PROXY`/`NO_PROXY` are honoured.

## Stored outputs

`clean`, `run` and `proxy` outputs cannot be produced again (their input was a pipe, a command's output or another server's reply), so they are stored on disk under an id: `clean` and `run` replies start with `[r:ID]`, and the proxy adds an item `[r:ID shortened by zazzle]` after each text it shortened (its own item, so JSON text stays JSON). When something was cut (a budget, a profile, progress bars, blobs, long lines), the reply says `read-again r:ID --full`.

- `zazzle read-again r:ID` returns the output exactly as it was shown; `--full` returns the uncut text with its line numbers (terminal escapes removed), with `--lines A-B` and `--budget T` (default 3000).
- The store is `~/.zazzle/outputs` (`$ZAZZLE_OUTPUTS_DIR`), mode 0700 with 0600 files. Each save reaps entries older than an hour; if the store then holds more than `$ZAZZLE_OUTPUTS_MAX_GB` gigabytes (default 2), the oldest go until half the bytes remain. A failed save keeps the output and says it was not stored.

## zazz scripts

Agents (and humans) can ship small programs to zazzled: a `.zazz` file is a ZIP (stored or deflate, read with built-in code) holding `zazz.json`, Rust sources (`src/`, `tests/`) or a prebuilt WASM module. Design and decisions: `ZAZZ-PLAN.md`; agent how-to: `agents/skills/zazz/SKILL.md`.

| command | does |
|---|---|
| `zazz new NAME` | a script directory to start from (manifest, `src/main.rs`, `tests/basic.rs`) |
| `zazz test DIR\|FILE [--update]` | builds to WASM and runs the tests; replies with diagnostics or a test report; registers nothing |
| `zazz pack DIR [-o FILE]` | a deterministic `.zazz` (same sources, same bytes, same id) |
| `zazz add FILE` | reads the file once, ids it by the BLAKE3 of its exact bytes, builds and tests once, registers; `cached` when known |
| `zazz call ID [JSON\|-]` / `zazz call ID OP [--params J] [--body J]` | runs `json-command` or one OpenAPI operation in a warm sandboxed worker; JSON in, JSON out |
| `zazz describe\|ls\|rm` | what a script is and may touch; the registry |
| `zazz install FILE\|ID [--as NAME]`, `zazz run NAME ARGS`, `uninstall`, `list` | commands for humans: `~/.zazzle/bin/NAME` links to the invoker; flags come from the manifest's `cli` spec |
| `zazz keygen NAME [--native] [--network ...]`, `zazz sign FILE`, `zazz trust add\|ls\|rm` | signing authorities and the machine's trust in them |
| `zazz doctor` | rustc, the `wasm32-wasip1` target, zazz-worker, the OS sandbox, paths |

- **Runtime:** Rust scripts compile (std + the embedded `zazz` SDK, built once per toolchain) to `wasm32-wasip1` and run in `zazz-worker` (wasmtime). Each call gets a fresh instance with time and memory limits; workers are kept warm per (script, directory, policy) and recycled.
- **Two sandbox layers:**
  - WASI preopens only the manifest's paths; the `zazz_host` imports carry the call, progress, cancellation and allowlisted HTTPS.
  - The OS sandbox wraps every worker, `rustc` build and test run: Landlock + seccomp on Linux, `sandbox-exec` on macOS. Unsigned scripts never run without it.
- **Signing:** a `zazz.sig` (ECDSA P-256 over a canonical digest, plus the signer's chain) is checked at add. Authorities are certificates in root-owned `/etc/zazzle/trust.d` (and the Keychain on macOS); their grants (network hosts, paths, native) narrow what the script may do. Verdicts are cached for `trust_ttl` (root-owned `/etc/zazzle/policy.json`, default 600 s) and refreshed in the background, so a call never waits on trust. Only signed and trusted scripts get network; scripts trusted for native code also get a host build and run natively under the OS sandbox, falling back to WASM when trust is withdrawn.
- **Streams:** through the invoker a script reads and writes the caller's real stdin, stdout and stderr; over MCP the output is captured into the reply.
- **Store:** `~/.zazzle/v1/zazz/` (0700) keeps compiled modules, native binaries and metadata, never the `.zazz` itself.

## Session ids and dedupe

With `ZAZZLE_SESSION` set (or over MCP, where each connection is a session) every response starts with `[r:ID]`. Repeating a request whose response is identical and was sent within the last 40,000 estimated tokens returns `[r:ID] already sent: zazzle read a.rs:1-20, unchanged. If you no longer have it: read-again r:ID`. Errors are not tagged, but their fixed parts are sent once per session in the same window: a repeated usage line becomes `usage: as above`, a repeated `similar:` list `similar: as listed earlier`, and a repeated `C# indexer unavailable: WHY` (whatever its age) `C# indexer still unavailable, as reported earlier; other tools work without it.`

`zazzle read-again r:ID` (MCP `zazzle_read_again`) runs the request behind a session response again, in its original working directory: the same text under the same id when nothing changed, else the current version under a new id with `r:OLD changed since it was sent`. So an agent can drop a response from its context (or lose it to compaction) and get it back exactly. The session keeps the last 512 requests; `clean`, `run` and `proxy` outputs come from the output store instead (see [Stored outputs](#stored-outputs)). Responses read again never reveal more than the original request did: a terse view stays without a sha. Edits are never deduplicated. Without a session there are no ids and no dedupe. State lives in daemon memory only.

## Cache

The daemon keeps derived data only, never file text, in `~/.zazzle/v1/` (`ZAZZLE_CACHE_DIR`); design notes: [src/.dev/decisions.json](src/.dev/decisions.json), file format: [src/.dev/contracts.json](src/.dev/contracts.json).

## Measurements

Linux x86_64 VM, p50 per process spawn over 3000 runs, microseconds (`macos/c/zazzle/bench/run.sh`): floor (`_exit`) 120, `zazzle version` 209, `zazzle read` 228 vs `cat` 1057, `zazzle find` on a small tree 554 vs `grep -rn` 1278, libc hello 941. On a 111 MB tree of 6,751 files: `find` 25-35 ms vs `grep -rn` 150 ms; `ls` with line counts 1088 ms cold, 52 ms after a daemon restart; walk share of `find` fell from 33 ms to 10 ms with the validated listing cache (the decision this supports: [src/.dev/decisions.json](src/.dev/decisions.json)). macOS (GitHub Actions virtual M1, noisy, 3000 runs, p50): floor 1734, libc hello 1662, `zazzle version` 1799, `zazzle read` 1736 vs `cat` 1938, `zazzle find` on a small tree 2361 vs `grep -rn` 2471, so `zazzle` costs about what any libSystem program costs and the gain there is in output size, not startup. On macOS the floor is dyld plus libSystem ([why](../../c/zazzle/.dev/decisions.json)), so the invoker can match but not beat a libSystem hello; the MCP front end avoids the spawn entirely.

## Safety

The daemon answers only the same uid (`SO_PEERCRED` / `getpeereid`; the invoker checks the daemon too), the socket is mode 0600, requests are size-limited, and edits refuse paths outside the caller's working directory after resolving symlinks. Reads are allowed anywhere the user can read. Hard-linked files are rewritten in place; extended attributes are not preserved by the atomic replace.

## Limits

Regex is the Rust dialect (no look-around, no backreferences). `zazzle` itself supports macOS arm64 and Linux x86_64/aarch64; on Intel Macs use `zazzled mcp` or `zazzled run`. Outlines are heuristic for languages without a grammar and for files over 1 MiB (tree-sitter would take about 1.5 s on an 8.7 MB generated parser). How parallel work is run (one thread pool per daemon) and why: [src/.dev/decisions.json](src/.dev/decisions.json). `agent-fs` (`afs`) is a separate tool in this repository with overlapping goals.

## Tests

`cargo test -p zazzled`; branch coverage (nightly, test code excluded, target 95%): `sh zazzled/coverage.sh 95` from `macos/rust` (currently 100%). C invoker: `make unit coverage smoke` in `macos/c/zazzle` (branch coverage of the invoker logic, 100%).

Why each dependency: [.dev/decisions.json](.dev/decisions.json).
