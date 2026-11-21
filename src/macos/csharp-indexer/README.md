# csharp-indexer

The C# side of zazzle: a per-user daemon that keeps an index of git work trees current from file-change notifications and answers `daemon` over a unix socket. It offers agents no tools of its own: they use zazzle for everything, and zazzle asks the indexer for C# terse views and structure. Every call is logged to `usage.jsonl` so the next version can be shaped by real use.

## Quick start
```bash
zazzle setup --hook                   # instead, for every client and project at once: zazzle in Claude Code, the Claude desktop app and Codex, and this hook (see daemon's README)
zazzle csharp-indexer init            # in one project: writes .mcp.json and .claude/settings.json (the zazzle server, its permission, the redirect hook)
zazzle csharp-indexer report          # after some use: what to improve next
zazzle csharp-indexer status          # is the shared daemon running, which roots it holds, its clients and memory
zazzle csharp-indexer restart         # after an upgrade: replace the running daemon with this version
zazzle csharp-indexer uninit          # in that project again: take out what init wrote
```
`zazzle` is the front door: agents read, search, list and edit files through it, and ask it for structure (`zazzle_overview`, `zazzle_find_symbol`, `zazzle_changed_since`, `zazzle_git_log`, `zazzle_git_blame`, `zazzle_report_gap`), which it answers from this indexer. `init` registers `zazzle` (`zazzle daemon mcp`, found beside the indexer or on `PATH`), removes an earlier registration of this indexer's own server, and prints a snippet for `AGENTS.md`/`CLAUDE.md`. The Claude Code hook (`zazzle csharp-indexer hook`) blocks `grep`/`rg`/`find`/`cat`/`head`/`tail`/`sed -n`, the built-in Grep and Glob tools, and the built-in Read on `.cs` files, and points at the matching zazzle tool; add `# allow-raw` to a command to bypass it (logged).

## Commands
Run as `zazzle csharp-indexer COMMAND`; `help` (or `--help`, `-h`) lists them, and an unknown command or argument exits 2.
- `init [--dir DIR]` wires zazzle and the redirect hook into a project (default: the current folder).
- `uninit [--dry-run] [DIR]` removes only what `init` wrote: the `zazzle` server in `.mcp.json` if it is the one init writes (stdio, a command ending in `daemon`, args `["mcp"]`), the redirect hook entry, the `mcp__zazzle` permission and the `zazzle` entry of `enabledMcpjsonServers` in `.claude/settings.json`, and any object or array that leaves empty. Every other byte of both files stays; a file left with nothing in it is deleted (and `.claude/` if that empties it). It prints each change (`--dry-run`: what it would change, writing nothing) and is safe to run twice.
- `status [--json]` whether the shared daemon runs: pid, version, uptime, socket, state folder, memory, connected clients and the roots it has indexed with their file counts and age. Exit 0 when it runs, 3 when it does not (still printed), 1 when it does not answer. `--json` gives one object: `tool` (`csharp-indexer`), `version` (this executable's), `ok`, `running`, `socket`, `state_dir`, then the daemon's `pid`, `daemon_version`, `started`, `uptime_seconds`, `draining`, `memory`, `clients` and `roots`. A daemon from before this command is reported with its version and the note that details need a newer daemon.
- `stop [--timeout SECONDS]` makes the daemon drain as it does for a newer version (new clients are refused, connected ones finish), waits up to the timeout (default 10 s), then kills it.
- `restart [--timeout SECONDS]` stops the daemon (if one runs), then starts a fresh one from this executable and prints both versions and pids.
- `serve [--root name=path]...` the daemon's MCP session over stdio (default root: current directory), for debugging; it lists no tools. Connects to the shared daemon, starting it if needed; `--no-daemon` or `ZAZZLE_INDEXER_NO_DAEMON=1` serves in-process.
- `daemon` the shared per-user daemon on a unix socket in the state directory; exits after 30 idle minutes (`ZAZZLE_INDEXER_IDLE_MINUTES`).
- `report`, `hook` (the Claude Code hook `init` installs), `--version`.

## Internal tools
`tools/list` is empty. `daemon` calls these by name over the daemon socket (`src/macos/daemon/src/indexer.rs`); agents reach them through zazzle:

| tool | zazzle | what |
|---|---|---|
| `view` | `zazzle_read` of a `.cs` file (`zazzle read` on the command line), and of a Razor file's `@code`/`@functions` blocks, which the daemon passes as `text` padded to the file's line numbers | terse behavioural view (usings, logging, regions, field-assigning constructors and log-and-rethrow catches hidden or collapsed; access modifiers dropped; `var` where the type is spelled out; long local variable names shortened to initials; multi-line XML doc comments cut to their first summary sentence (`docs` argument or `ZAZZLE_INDEXER_DOCS`: `full`, `summary` (default), `none`); `// ------` banners hidden; strings of 240+ characters become `"…"(N chars)`; collection initialisers of 8+ items over 6+ lines become `{ N items }`; bodies of `<auto-generated>` files fold to `{ … N lines }`), keeping source line numbers and giving no hash. A view that would save under 3% is refused with `no_gain` so the daemon shows the source instead (`force` overrides) |
| `overview` | `zazzle_overview` | languages, top directories, production and test projects, entry points, largest files, from the index; honours the root's `.ignore`, and answers with partial counts if the index is still building after 3 s |
| `find_symbol` | `zazzle_find_symbol` | declarations by name, or by a dotted `Namespace.Type.Member` (any trailing part) whose leading parts must end the container; C# parsed with Roslyn (exact), other languages by a regex extractor |
| `refs` | `zazzle refs` (C#) | exact references from Roslyn's SymbolFinder over an in-memory solution of the root's `.cs` files (rebuilt when the index changes): calls through interfaces, every overload, both halves of partial types; `not_found` when no C# declaration has the name, and zazzle then answers by text |
| `changed_since` | `zazzle_changed_since` | changes since a cursor (valid within one daemon run) |
| `git_log`, `git_blame` | `zazzle_git_log`, `zazzle_git_blame` | compact git history and authorship |
| `report_gap` | `zazzle_report_gap` | the agent says what it could not get; stored in the log |

The indexer's former `search`, `read`, `find_files`, `outline`, `run` and `find_references` are gone: `zazzle_find`, `zazzle_read`, `zazzle_ls`, `zazzle_outline`, `zazzle run` and `zazzle_refs` replace them. Paths in and out are `root:relative/path` (daemon rewrites them for the agent). Every root must be inside a git work tree.

## How it works
The parts are in [.dev/component-diagram.json](.dev/component-diagram.json) and [src/.dev/component-diagram.json](src/.dev/component-diagram.json), and the design (content-addressed store, stat key, change tracking, labelled answers) in the `decisions.json` of [src](src/.dev/decisions.json), [src/Index](src/Index/.dev/decisions.json), [src/Analysis](src/Analysis/.dev/decisions.json) and [src/Tools](src/Tools/.dev/decisions.json). The rules of the `view` tool are in [src/View/.dev/feature.json](src/View/.dev/feature.json).

## Usage log
`~/Library/Application Support/csharp-indexer/usage.jsonl` on macOS, `$XDG_STATE_HOME/csharp-indexer/usage.jsonl` elsewhere (`ZAZZLE_INDEXER_STATE_DIR` overrides; the store and socket live there too). Text arguments are hashed unless `ZAZZLE_INDEXER_LOG_PLAIN=1`; answers are represented only by sizes and hashes. The daemon also records the final response after session dedupe in `~/.zazzle/usage.jsonl` (`ZAZZLE_USAGE_LOG` overrides; set it empty to disable). `report` combines both logs to show source-to-view compaction and final-delivery savings. Fields and rotation: [src/.dev/observability.json](src/.dev/observability.json). All settings: [src/.dev/configuration.json](src/.dev/configuration.json).

## Known limits
- macOS is the primary target. FSEvents replay is implemented and covered by a macOS-only test, which CI (Linux only) skips and only the release runs, on the owner's Mac; it was not run on a Mac when written. Linux uses `FileSystemWatcher` without replay (full reconcile at start). Windows is untested.
- Local filesystems only.
- The stat key has no ctime (not exposed by .NET): an in-place rewrite that keeps size, mtime and birth time is caught by the watcher but not by a restart reconcile.
- Ranking is path order; there is no relevance ranking and no import/dependency graph.
- `find_symbol` is name-based, not type-resolved.
- The published file is large (about 130 MB with Roslyn, ReadyToRun, untrimmed). The store is about twice the source size for dense code. Initial indexing of 20,000 files (80 MB) took about 12 s, measured when the index also held search filters.
- MCP `roots/list` is not used: daemon names the git work tree of its working directory, and `serve` uses `--root` or its own working directory.

## Build (run in `src/macos/csharp-indexer/`)
- Test: `dotnet test tests`
- Single test: `dotnet test tests --filter test_find_symbol`
- Publish: `dotnet publish src -c Release -r osx-arm64 -o out` (also `osx-x64`, `linux-x64`, `linux-arm64`)
- Release: it ships inside the main `v*` release (`release.sh` in [continuous_integration_and_delivery](https://github.com/david-brandt-the-icelander/continuous_integration_and_delivery/tree/main/zazzle)), gzipped per CPU type in the `zazzle` Homebrew formula, which expands the right one on install; the Linux tarballs carry it too. Binaries are unsigned apart from the SDK's ad-hoc signature on macOS.
