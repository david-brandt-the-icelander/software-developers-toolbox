# codeindexd

Single-file MCP server that answers code questions over one or more git work trees, so agents use it instead of grep, find, cat and sed. Every call is logged to `usage.jsonl` to guide later versions.

## Status
Milestone 1 (walking skeleton): stdio MCP, tools `find_files`, `read`, `search`, usage log. Every answer is a fresh scan (`path: scan`, `reason: index_not_built`); the index, git blob-id store, watcher and daemon come in later milestones.

## Use
```bash
codeindexd serve --root name=/path/to/repo [--root other=/path/to/other]
```
Without `--root` the current directory is the only root. Each root must be inside a git work tree. Paths in tools are `root:relative/path`.

## Tools
- `find_files`: name substring and glob, paged.
- `read`: line range with line numbers, token budget, continuation hint.
- `search`: .NET regex per line (or `literal`), glob filter, context, paging by `offset`.

## Usage log
`~/Library/Application Support/codeindexd/usage.jsonl` on macOS, `$XDG_STATE_HOME/codeindexd/usage.jsonl` elsewhere (`CODEINDEXD_STATE_DIR` overrides). Text arguments are stored as 12-hex SHA-256 prefixes; set `CODEINDEXD_LOG_PLAIN=1` to keep them readable. Fields: `v ts session tool args path reason ms bytes_out est_tokens results truncated error`.

## Build (run in `macos/dotnet/codeindexd/`)
- Test: `dotnet test tests`
- Single test: `dotnet test tests --filter test_search_context_and_offset`
- Publish: `dotnet publish src -c Release -r osx-arm64 -o out` (also `osx-x64`, `linux-x64`, `linux-arm64`, `win-x64`)
