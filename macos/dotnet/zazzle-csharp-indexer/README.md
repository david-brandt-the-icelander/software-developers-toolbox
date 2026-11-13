# zazzle-csharp-indexer

The C# side of zazzle: a per-user daemon that keeps an index of git work trees current from file-change notifications and answers `zazzled` over a unix socket. It offers agents no tools of its own: they use zazzle for everything, and zazzle asks the indexer for C# terse views and structure. Every call is logged to `usage.jsonl` so the next version can be shaped by real use.

## Quick start
```bash
zazzle-csharp-indexer init            # in a project: writes .mcp.json and .claude/settings.json (the zazzle server, its permission, the redirect hook)
zazzle-csharp-indexer report          # after some use: what to improve next
```
`zazzle` is the front door: agents read, search, list and edit files through it, and ask it for structure (`zazzle_overview`, `zazzle_find_symbol`, `zazzle_changed_since`, `zazzle_git_log`, `zazzle_git_blame`, `zazzle_report_gap`), which it answers from this indexer. `init` registers `zazzle` (`zazzled mcp`, found beside the indexer or on `PATH`), removes an earlier registration of this indexer's own server, and prints a snippet for `AGENTS.md`/`CLAUDE.md`. The Claude Code hook (`zazzle-csharp-indexer hook`) blocks `grep`/`rg`/`find`/`cat`/`head`/`tail`/`sed -n`, the built-in Grep and Glob tools, and the built-in Read on `.cs` files, and points at the matching zazzle tool; add `# allow-raw` to a command to bypass it (logged).

## Commands
- `serve [--root name=path]...` the daemon's MCP session over stdio (default root: current directory), for debugging; it lists no tools. Connects to the shared daemon, starting it if needed; `--no-daemon` or `ZAZZLE_INDEXER_NO_DAEMON=1` serves in-process.
- `daemon` the shared per-user daemon on a unix socket in the state directory; exits after 30 idle minutes (`ZAZZLE_INDEXER_IDLE_MINUTES`).
- `init [--dir path]`, `report`, `hook`, `--version`.

## Internal tools
`tools/list` is empty. `zazzled` calls these by name over the daemon socket (`macos/rust/zazzled/src/indexer.rs`); agents reach them through zazzle:

| tool | zazzle | what |
|---|---|---|
| `view` | `zazzle_read` of a `.cs` file (`zazzle read` on the command line) | terse behavioural view (usings, logging, regions, field-assigning constructors and log-and-rethrow catches hidden or collapsed; access modifiers dropped; `var` where the type is spelled out; long local variable names shortened to initials), keeping source line numbers and giving no hash |
| `overview` | `zazzle_overview` | languages, top directories, entry points, largest files, from the index |
| `find_symbol` | `zazzle_find_symbol` | declarations by name; C# parsed with Roslyn (exact), other languages by a regex extractor |
| `changed_since` | `zazzle_changed_since` | changes since a cursor (valid within one daemon run) |
| `git_log`, `git_blame` | `zazzle_git_log`, `zazzle_git_blame` | compact git history and authorship |
| `report_gap` | `zazzle_report_gap` | the agent says what it could not get; stored in the log |

The indexer's former `search`, `read`, `find_files`, `outline`, `run` and `find_references` are gone: `zazzle_find`, `zazzle_read`, `zazzle_ls`, `zazzle_outline`, `zazzle run` and `zazzle_refs` replace them. Paths in and out are `root:relative/path` (zazzled rewrites them for the agent). Every root must be inside a git work tree.

## How it works
The parts are in [.dev/component-diagram.json](.dev/component-diagram.json) and [src/.dev/component-diagram.json](src/.dev/component-diagram.json), and the design (content-addressed store, stat key, change tracking, labelled answers) in the `decisions.json` of [src](src/.dev/decisions.json), [src/Index](src/Index/.dev/decisions.json), [src/Analysis](src/Analysis/.dev/decisions.json) and [src/Tools](src/Tools/.dev/decisions.json). The rules of the `view` tool are in [src/View/.dev/feature.json](src/View/.dev/feature.json).

## Usage log
`~/Library/Application Support/zazzle-csharp-indexer/usage.jsonl` on macOS, `$XDG_STATE_HOME/zazzle-csharp-indexer/usage.jsonl` elsewhere (`ZAZZLE_INDEXER_STATE_DIR` overrides; the store and socket live there too). Text arguments are hashed unless `ZAZZLE_INDEXER_LOG_PLAIN=1`; fields and rotation: [src/.dev/observability.json](src/.dev/observability.json). All settings: [src/.dev/configuration.json](src/.dev/configuration.json).

## Known limits
- macOS is the primary target. FSEvents replay is implemented and covered by a macOS-only test that runs in CI, but was not run on a Mac when written. Linux uses `FileSystemWatcher` without replay (full reconcile at start). Windows is untested.
- Local filesystems only.
- The stat key has no ctime (not exposed by .NET): an in-place rewrite that keeps size, mtime and birth time is caught by the watcher but not by a restart reconcile.
- Ranking is path order; there is no relevance ranking and no import/dependency graph.
- `find_symbol` is name-based, not type-resolved.
- The published file is large (about 130 MB with Roslyn, ReadyToRun, untrimmed). The store is about twice the source size for dense code. Initial indexing of 20,000 files (80 MB) took about 12 s, measured when the index also held search filters.
- MCP `roots/list` is not used: zazzled names the git work tree of its working directory, and `serve` uses `--root` or its own working directory.

## Build (run in `macos/dotnet/zazzle-csharp-indexer/`)
- Test: `dotnet test tests`
- Single test: `dotnet test tests --filter test_find_symbol`
- Publish: `dotnet publish src -c Release -r osx-arm64 -o out` (also `osx-x64`, `linux-x64`, `linux-arm64`)
- Release: it ships inside the main `v*` release (`.github/workflows/release.yml`), gzipped per CPU type in the `zazzle` Homebrew formula, which expands the right one on install; the Linux tarballs carry it too. Binaries are unsigned apart from the SDK's ad-hoc signature on macOS.
