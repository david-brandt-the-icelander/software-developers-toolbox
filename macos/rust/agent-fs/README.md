# agent-fs (`afs`)

JSON-in, JSON-out replacement for the `cat`, `sed -n`, `grep -r`, `git grep` and `find` calls agents repeat. Most of an agent's context cost is re-reading the same output, so `afs` remembers what each session has already received and sends only what changed.

Build: `cargo build --release -p agent-fs` in `macos/rust` (Linux and macOS).

```bash
afs call '{"op":"read","path":"src/main.rs","ranges":[[1,40]]}'
afs call '{"op":"search","pattern":"TODO","glob":["*.rs"],"context":1}'
afs call '[{"op":"list","path":"src"},{"op":"outline","paths":["src"]}]'   # batch: one reply per request
afs serve    # MCP server over stdio (one session per process)
afs jsonl    # one request per stdin line, one reply per stdout line (one session)
```

`afs call` is one-shot, so it has no session memory; use `serve` or `jsonl` to get `unchanged` and diff replies. Replies always carry `ok`; failures are `{"ok":false,"error":...}` and exit code 1.

## Operations

| op | purpose | main fields |
|---|---|---|
| `read` | cat, head, sed -n | `path`, `ranges:[[a,b]]` (1-based), `max_lines` (400), `max_bytes` (16384), `if_none_match`, `fresh` |
| `search` | grep -r, git grep, rg | `pattern`, `literal`, `ignore_case`, `paths`, `glob` (`!x` excludes), `context`, `max_matches` (100), `files_only`, `fields` (`line`,`col`,`text`), `hidden`, `no_ignore`, `fresh` |
| `list` | ls, find | `path`, `glob`, `depth` (1 unless glob), `long`, `max_entries` (500), `cursor`, `hidden`, `no_ignore` |
| `stat` | stat, shasum | `path`, `hash` |
| `outline` | grep for definitions | `paths`, `max_symbols` (300); rs py js ts go java kt cs swift c cpp sh md |
| `diff_since` | re-reading a changed file | `path`, `etag` (a version this session received) |

## Token savers

- **Unchanged**: repeating a read or search whose answer was already sent returns `{"unchanged":true,"etag","ref"}` with no content. `fresh:true` forces the content again.
- **Diff**: re-reading a whole file that changed since this session fully received it returns `hunks` (`at` = 1-based old line, `del` = old lines removed, `ins` = new text) when smaller than the file. Apply hunks from the last `at` to the first.
- **Caps**: output is bounded; `truncated`, `next_ranges` (read) or `next_cursor` (list) and `bytes_omitted` say how to continue. Lines over 2000 bytes are cut (`long_lines_cut`). Binary files report `binary:true`.
- **Compact results**: search groups by file, merges overlapping context, and `fields:["line"]` returns bare number arrays.

## Caching

- A file's etag is the first 64 bits of its BLAKE3 hash. Stamps (device, inode, size, mtime, ctime in ns) let a repeat skip hashing; a stamp younger than 2 seconds is never trusted (the "racy" rule git uses), so same-tick edits cannot serve stale data.
- Search, list and outline results are cached with the stamp of every file and directory the walk visited, plus each directory's `.gitignore` and `.ignore`. A hit re-stats those paths (no reads, no regex work) and recomputes on any difference.
- Session memory is bounded: 64 MiB of file versions kept as diff bases (files over 8 MiB are not kept), 256 cached results.

## I/O

Whole-file reads through `read(2)` with `O_NOATIME` and `posix_fadvise(SEQUENTIAL)` on Linux; ripgrep's regex and `memchr` (runtime-detected SIMD) for matching; the `ignore` crate's parallel walker. `mmap` is deliberately not used: truncating a mapped file raises SIGBUS and would kill a long-lived server.

## Not implemented

Planned, but absent until a benchmark justifies them: inotify/fanotify/FSEvents dirty sets (staleness is detected by re-stat instead), a trigram index, `io_uring` batching, tree-sitter outlines (outlines are line regexes and can miss multi-line signatures), hand-written assembly (the compiler-vectorised `memchr` already saturates memory bandwidth on the hot loops). `search` patterns are matched across the whole buffer in multi-line mode, and a match is reported on the line where it starts.

## Dependencies

`blake3` hashing; `ignore` gitignore-aware parallel walking; `regex` and `memchr` matching; `similar` line diffs; `serde`/`serde_json` the JSON protocol; `clap` the CLI; `libc` Linux read hints; `tempfile` (tests) fixture directories.

## Use from an agent

Claude Code, in `.mcp.json`: `{"mcpServers":{"afs":{"command":"afs","args":["serve"]}}}`. Then tell the agent to prefer the `afs_*` tools over shell reads (see `agents/skills/agent-fs/SKILL.md`).
