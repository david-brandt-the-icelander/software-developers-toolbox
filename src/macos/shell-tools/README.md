# shell-tools (library)

The JSON file operations of zazzle's daemon: `read`, `search`, `list`, `stat`, `outline` and `diff_since`, answered as JSON, with memory of what each session was already sent so a repeat costs an `unchanged` marker and a changed file costs only its diff. It is a library crate (`shell_tools`) with no program of its own; the daemon (`src/macos/daemon`) runs it for `zazzle call` and `zazzle stat` and for the MCP tools `zazzle_call` and `zazzle_stat`.

```bash
zazzle call '{"op":"read","path":"src/main.rs","ranges":[[1,40]]}'
zazzle call '{"op":"search","pattern":"TODO","glob":["*.rs"],"context":1}'
zazzle call '[{"op":"list","path":"src"},{"op":"outline","paths":["src"]}]'   # batch: one reply per request
echo '{"op":"stat","path":"Cargo.toml","hash":true}' | zazzle call -
zazzle stat Cargo.toml src --hash                                            # the same stat as text
```

Replies always carry `ok`; failures are `{"ok":false,"error":...}` and make `zazzle call` exit 1. Paths are relative to the caller's working directory.

## Operations

| op | purpose | main fields |
|---|---|---|
| `read` | cat, head, sed -n | `path`, `ranges:[[a,b]]` (1-based), `max_lines` (400), `max_bytes` (16384), `if_none_match`, `fresh` |
| `search` (or `find`) | grep -r, git grep, rg | `pattern`, `literal`, `ignore_case`, `paths`, `glob` (`!x` excludes), `context`, `max_matches` (100), `files_only`, `fields` (`line`,`col`,`text`), `hidden`, `no_ignore`, `fresh` |
| `list` (or `ls`) | ls, find | `path`, `glob`, `depth` (1 unless glob), `long`, `max_entries` (500), `cursor`, `hidden`, `no_ignore`, `fresh` |
| `stat` | stat, shasum | `path`, `hash` |
| `outline` | grep for definitions | `paths`, `max_symbols` (300), `fresh`; returns `[start, end, kind, name]` per symbol, nested names dotted (`A.go`) |
| `diff_since` | re-reading a changed file | `path`, `etag` (a version this session received) |

## Token savers

- **Unchanged**: within a session, repeating a read, search, list or outline whose answer was already sent returns `{"unchanged":true,"etag","ref"}` with no content. `fresh:true` forces the content again. Without a session (no `ZAZZLE_SESSION`, not over MCP) nothing is remembered.
- **Diff**: re-reading a whole file that changed since this session fully received it returns `hunks` (`at` = 1-based old line, `del` = old lines removed, `ins` = new text) when smaller than the file. Apply hunks from the last `at` to the first.
- **Caps**: output is bounded; `truncated`, `next_ranges` (read) or `next_cursor` (list) and `bytes_omitted` say how to continue. Lines over 2000 bytes are cut (`long_lines_cut`). Binary files report `binary:true`.
- **Compact results**: search groups by file, merges overlapping context, and `fields:["line"]` returns bare number arrays.

## Where the work is done

The library parses requests, keeps the session memory and renders replies; the daemon lends it, through the `Host` trait (`src/host.rs`), the parts it already has: its stat-validated walk cache, `find`'s parallel scan and its outlines (tree-sitter through language modules). So `search` skips binary files and files over 2 MiB and counts matching lines, and `outline` covers every language the daemon's `outline` does. How the merge was decided, with the benchmark numbers, is in [src/.dev/decisions.json](src/.dev/decisions.json).

## Memory

Each session's table of what it was sent is its own (64 sessions kept, least recently used dropped); file versions kept as diff bases (64 MiB, files over 8 MiB not kept) and the trusted file stamps (the 2-second racy rule) are shared by all sessions. Details: [src/.dev/component-diagram.json](src/.dev/component-diagram.json).

## Compatibility with the old `shell-tools` program

Requests that worked with `shell-tools call` work with `zazzle call`, with these differences: the program and its `serve` (MCP, `afs_*` tools) and `jsonl` modes are gone, replaced by `zazzle call` and the `zazzle_call` tool; `search` counts matching lines, not matches, in `total_matches`, and skips files over 2 MiB; replies no longer carry `cached`; `list` shows only regular files and directories (as the walk cache sees them) and refuses a file path; `outline` reports what the daemon's outline finds, so kinds and ranges can differ from the old grammars; `AFS_BATCH_IO` is gone.

## Testing

`cargo test -p shell-tools` runs the unit tests beside the code against a plain file-system `Host`; the daemon's `src/cmd/call.rs` tests run the operations through the real daemon services. CI gates `shell-tools` at 95% line and 95% branch coverage (`cargo +nightly llvm-cov --branch -p shell-tools --summary-only`). What each test checks is in [src/.dev/verification.json](src/.dev/verification.json).

## Dependencies

What each crate is used for is in [.dev/references.json](.dev/references.json).
