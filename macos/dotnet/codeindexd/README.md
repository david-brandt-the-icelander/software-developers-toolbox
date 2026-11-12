# codeindexd

Single-file MCP server that answers code questions for agents over one or more git work trees, so they use it instead of grep, find, cat and sed. A per-user daemon keeps an index current from file-change notifications; every call is logged to `usage.jsonl` so the next version can be shaped by real use.

## Quick start
```bash
codeindexd init            # in a project: writes .mcp.json and .claude/settings.json (server, permission, redirect hook)
codeindexd report          # after some use: what to improve next
```
`init` prints a snippet for `AGENTS.md`/`CLAUDE.md`. The Claude Code hook (`codeindexd hook`) blocks `grep`/`rg`/`find`/`cat`/`head`/`tail`/`sed -n`, the built-in Grep and Glob tools, and points at the matching MCP tool; add `# allow-raw` to a command to bypass it (logged).

## Commands
- `serve [--root name=path]...` stdio MCP server (default root: current directory). Connects to the shared daemon, starting it if needed; `--no-daemon` or `CODEINDEXD_NO_DAEMON=1` serves in-process.
- `daemon` the shared per-user daemon on a unix socket in the state directory; exits after 30 idle minutes (`CODEINDEXD_IDLE_MINUTES`).
- `init [--dir path]`, `report`, `hook`, `--version`.

## Tools
- `overview`, `outline`, `find_symbol`, `find_references`: structure from the index. C# is parsed with Roslyn (exact); other languages use a regex extractor and are marked `[heuristic]`.
- `search`: .NET regex per line (or `literal`), glob filter, `mode` lines/files/count, context, paging. Patterns .NET rejects fall back to `rg -P` or `git grep -P`.
- `read`: line ranges with numbers and a token budget; an unchanged repeat read returns a one-line note.
- `find_files`, `changed_since` (cursor-based, valid within one daemon run), `git_log`, `git_blame`.
- `run`: argv passthrough for read-only commands (grep, rg, sed, awk, find, cat, head, tail, wc, ls, git log/blame/show/diff/status ...). Best-effort guard against edits; `run.unrestricted: true` in `config.json` (or `CODEINDEXD_RUN_UNRESTRICTED=1`) lifts it.
- `report_gap`: the agent says what it could not get; stored in the log.

Paths are `root:relative/path`. Every root must be inside a git work tree.

## How it works
- Content-addressed store (SQLite, WAL) keyed by git blob id (SHA-1 of `blob <len>\0` + bytes, computed locally and equal to `git hash-object`). Line counts, language, trigram Bloom filter, symbols, identifier tables and digest are stored once per blob, so copies, worktrees and branches share them.
- Per-path stat key (size, mtime, birth time) avoids rehashing unchanged files. Files come from `git ls-files -co --exclude-standard`, so ignore rules are git's own.
- Change tracking: FSEvents on macOS (event id persisted, journal replayed after a restart if the last full scan is under 24 hours old), `FileSystemWatcher` elsewhere. Events are debounced, unknown paths are checked with `git check-ignore`, and a full reconcile runs on `.gitignore`/`HEAD` changes, watcher errors and every 5 minutes.
- Before `search`, `find_references` and `changed_since` answer, the touched files are re-stat'd (a "barrier"), so an edit made a moment ago is seen even if its event has not arrived.
- Search: required literals are extracted from the regex, their case-folded trigrams are tested against each file's Bloom filter, and only candidates are read and matched. Files too large (16 MiB) are listed but not analysed.
- Fallbacks are labelled in the log and the answer: `index_cold` (index still building, plain scan), `regex_unsupported` (external PCRE engine), `run:<prog>` (passthrough).

## Usage log
`~/Library/Application Support/codeindexd/usage.jsonl` on macOS, `$XDG_STATE_HOME/codeindexd/usage.jsonl` elsewhere (`CODEINDEXD_STATE_DIR` overrides; the store and socket live there too). Text arguments are stored as 12-hex SHA-256 prefixes (`CODEINDEXD_LOG_PLAIN=1` keeps them readable); `run` keeps the program and flags. Fields: `v ts session tool args path reason ms bytes_out est_tokens results truncated candidates files note error`.

## Known limits
- macOS is the primary target. FSEvents replay is implemented and covered by a macOS-only test that runs in CI, but was not run on a Mac when written. Linux uses `FileSystemWatcher` without replay (full reconcile at start). Windows is untested.
- Local filesystems only.
- The stat key has no ctime (not exposed by .NET): an in-place rewrite that keeps size, mtime and birth time is caught by the watcher but not by a restart reconcile.
- Ranking is path order; there is no relevance ranking and no import/dependency graph.
- `find_references` is name-based, not type-resolved.
- The published file is large (about 130 MB with Roslyn, ReadyToRun, untrimmed). The store is about twice the source size for dense code. Initial indexing of 20,000 files (80 MB) took about 12 s; afterwards a `search` answered in about 100 ms inside the daemon (about 0.15 s including starting the client process).
- MCP `roots/list` is not used; the default root is the working directory Claude Code launches the server in.

## Build (run in `macos/dotnet/codeindexd/`)
- Test: `dotnet test tests`
- Single test: `dotnet test tests --filter test_search_context_and_offset`
- Publish: `dotnet publish src -c Release -r osx-arm64 -o out` (also `osx-x64`, `linux-x64`, `linux-arm64`)
- Release: push a `codeindexd-v*` tag (workflow `release-codeindexd.yml`); the binaries are unsigned, so on macOS run `xattr -d com.apple.quarantine codeindexd` after download.
