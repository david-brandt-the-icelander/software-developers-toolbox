# zazzle-csharp-indexer

Single-file MCP server that answers code questions for agents over one or more git work trees, so they use it instead of grep, find, cat and sed. A per-user daemon keeps an index current from file-change notifications; every call is logged to `usage.jsonl` so the next version can be shaped by real use.

## Quick start
```bash
zazzle-csharp-indexer init            # in a project: writes .mcp.json and .claude/settings.json (zazzle and indexer servers, permissions, redirect hook)
zazzle-csharp-indexer report          # after some use: what to improve next
```
`zazzle` is the front door: agents read, search, list and edit files through it, and it asks the indexer for C# terse views. `init` registers both MCP servers (`zazzle` via `zazzled mcp`, found beside the indexer or on `PATH`, and this indexer for C# structure: `overview`, `find_symbol`, `find_references`, `changed_since`, git history) and prints a snippet for `AGENTS.md`/`CLAUDE.md`. The Claude Code hook (`zazzle-csharp-indexer hook`) blocks `grep`/`rg`/`find`/`cat`/`head`/`tail`/`sed -n`, the built-in Grep and Glob tools, and the built-in Read on `.cs` files, and points at the matching zazzle tool; add `# allow-raw` to a command to bypass it (logged).

## Commands
- `serve [--root name=path]...` stdio MCP server (default root: current directory). Connects to the shared daemon, starting it if needed; `--no-daemon` or `ZAZZLE_INDEXER_NO_DAEMON=1` serves in-process.
- `daemon` the shared per-user daemon on a unix socket in the state directory; exits after 30 idle minutes (`ZAZZLE_INDEXER_IDLE_MINUTES`).
- `init [--dir path]`, `report`, `hook`, `--version`.

## Tools
- `overview`, `outline`, `find_symbol`, `find_references`: structure from the index. C# is parsed with Roslyn (exact); other languages use a regex extractor and are marked `[heuristic]`.
- `search`: .NET regex per line (or `literal`), glob filter, `mode` lines/files/count, context, paging. Patterns .NET rejects fall back to `rg -P` or `git grep -P`.
- `read`: line ranges with numbers and a token budget; an unchanged repeat read returns a one-line note.
- `find_files`, `changed_since` (cursor-based, valid within one daemon run), `git_log`, `git_blame`.
- `run`: argv passthrough for read-only commands (grep, rg, sed, awk, find, cat, head, tail, wc, ls, git log/blame/show/diff/status ...). Best-effort guard against edits; `run.unrestricted: true` in `config.json` (or `ZAZZLE_INDEXER_RUN_UNRESTRICTED=1`) lifts it.
- `view`: terse behavioural view of a C# file (usings, logging, regions, field-assigning constructors and log-and-rethrow catches hidden or collapsed; access modifiers dropped; `var` where the type is spelled out; long local variable names shortened to initials), keeping source line numbers and giving no hash. `zazzle read` uses it for `.cs` files through the daemon socket.
- `report_gap`: the agent says what it could not get; stored in the log.

Paths are `root:relative/path`. Every root must be inside a git work tree.

## How it works
The parts are in [.dev/component-diagram.json](.dev/component-diagram.json) and [src/.dev/component-diagram.json](src/.dev/component-diagram.json), and the design (content-addressed store, stat key, change tracking, barrier, Bloom-filter search, labelled fallbacks) in the `decisions.json` of [src](src/.dev/decisions.json), [src/Index](src/Index/.dev/decisions.json), [src/Analysis](src/Analysis/.dev/decisions.json) and [src/Tools](src/Tools/.dev/decisions.json). The rules of the `view` tool are in [src/View/.dev/feature.json](src/View/.dev/feature.json).

## Usage log
`~/Library/Application Support/zazzle-csharp-indexer/usage.jsonl` on macOS, `$XDG_STATE_HOME/zazzle-csharp-indexer/usage.jsonl` elsewhere (`ZAZZLE_INDEXER_STATE_DIR` overrides; the store and socket live there too). Text arguments are hashed unless `ZAZZLE_INDEXER_LOG_PLAIN=1`; fields and rotation: [src/.dev/observability.json](src/.dev/observability.json). All settings: [src/.dev/configuration.json](src/.dev/configuration.json).

## Known limits
- macOS is the primary target. FSEvents replay is implemented and covered by a macOS-only test that runs in CI, but was not run on a Mac when written. Linux uses `FileSystemWatcher` without replay (full reconcile at start). Windows is untested.
- Local filesystems only.
- The stat key has no ctime (not exposed by .NET): an in-place rewrite that keeps size, mtime and birth time is caught by the watcher but not by a restart reconcile.
- Ranking is path order; there is no relevance ranking and no import/dependency graph.
- `find_references` is name-based, not type-resolved.
- The published file is large (about 130 MB with Roslyn, ReadyToRun, untrimmed). The store is about twice the source size for dense code. Initial indexing of 20,000 files (80 MB) took about 12 s; afterwards a `search` answered in about 100 ms inside the daemon (about 0.15 s including starting the client process).
- MCP `roots/list` is not used; the default root is the working directory Claude Code launches the server in.

## Build (run in `macos/dotnet/zazzle-csharp-indexer/`)
- Test: `dotnet test tests`
- Single test: `dotnet test tests --filter test_search_context_and_offset`
- Publish: `dotnet publish src -c Release -r osx-arm64 -o out` (also `osx-x64`, `linux-x64`, `linux-arm64`)
- Release: it ships inside the main `v*` release (`.github/workflows/release.yml`), gzipped per CPU type in the `zazzle` Homebrew formula, which expands the right one on install; the Linux tarballs carry it too. Binaries are unsigned apart from the SDK's ad-hoc signature on macOS.
