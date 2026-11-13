# agent-fs (`afs`)

JSON-in, JSON-out replacement for the `cat`, `sed -n`, `grep -r`, `git grep` and `find` calls agents repeat. Most of an agent's context cost is re-reading the same output, so `afs` remembers what each session has already received and sends only what changed.

Build: `cargo build --release -p agent-fs` in `macos/rust` (Linux and macOS), or download the universal macOS binary: `https://icelander.net/downloads/zazzle/latest/zazzle-macos-universal.tar.gz` (with a `.sha256` next to it; also on the GitHub releases). Or with Homebrew: `brew install zazzcat/z/zazzle` (installs every tool here). A browser download of an unsigned build makes macOS say it "could not verify" the binary; run `xattr -d com.apple.quarantine afs` after unpacking.

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
| `outline` | grep for definitions | `paths`, `max_symbols` (300); returns `[start, end, kind, name]` per symbol; tree-sitter for rs py js jsx mjs ts tsx go java c cpp (+ h hh hpp), line patterns for kt cs swift scala sh md |
| `diff_since` | re-reading a changed file | `path`, `etag` (a version this session received) |

`search` patterns are matched across the whole buffer in multi-line mode, and a match is reported on the line where it starts.

## Token savers

- **Unchanged**: repeating a read or search whose answer was already sent returns `{"unchanged":true,"etag","ref"}` with no content. `fresh:true` forces the content again.
- **Diff**: re-reading a whole file that changed since this session fully received it returns `hunks` (`at` = 1-based old line, `del` = old lines removed, `ins` = new text) when smaller than the file. Apply hunks from the last `at` to the first.
- **Caps**: output is bounded; `truncated`, `next_ranges` (read) or `next_cursor` (list) and `bytes_omitted` say how to continue. Lines over 2000 bytes are cut (`long_lines_cut`). Binary files report `binary:true`.
- **Compact results**: search groups by file, merges overlapping context, and `fields:["line"]` returns bare number arrays.

## Caching

Session memory is bounded (64 MiB of file versions kept as diff bases, files over 8 MiB not kept, 256 cached results); how etags, stamps, the 2-second racy rule and the result cache work is in [src/.dev/component-diagram.json](src/.dev/component-diagram.json) and [src/.dev/decisions.json](src/.dev/decisions.json).

## Outline

`outline` parses with tree-sitter and returns every item with its exact line range, so an agent can `read` just `[start, end]` instead of the file. Names inside containers (impl/trait/mod, class, namespace, interface bodies) are dotted: `A.go`, `n.K.m`; Rust `impl` items read `Trait for Type`; JS/TS `const f = () => ...` counts as a function. Function bodies are not entered, so locals and nested functions are omitted. Broken syntax still yields the items that parse. Files over 2 MiB are skipped, and languages without a grammar (kt cs swift scala sh md) use line patterns where `end` equals `start`.

## Trigram index

From the second `search` on the same paths, globs and flags, a `serve` or `jsonl` session reads only the files an in-memory trigram index says can match, with results equal to a full scan; how it is built, kept up to date and dropped is in [src/.dev/TriIndex.state-diagram.json](src/.dev/TriIndex.state-diagram.json).

## I/O

Files are read whole with `read(2)`, never `mmap`, and search and outline load them in batches through io_uring (Linux) or POSIX AIO (macOS) only when each process finds that faster (on a 4-core VM with a hot page cache io_uring was about 1.6x *slower* than plain reads for 7k small files); the design is in [src/.dev/decisions.json](src/.dev/decisions.json), and `AFS_BATCH_IO=always|never` (default `auto`) in [src/.dev/configuration.json](src/.dev/configuration.json).

## Testing

`cargo test -p agent-fs` runs unit tests beside the code and `tests/cli.rs`, which drives the real binary (arguments, stdin, exit codes, `jsonl`, MCP). CI gates `agent-fs` at 95% line and 95% branch coverage on Linux (`cargo +nightly llvm-cov -p agent-fs --branch`); measured at the time of writing: 99.0% lines, 98.7% branches (294 of 298), 98 unit tests plus 9 CLI tests. The macOS-only `aio.rs` is not compiled on Linux, so it is covered only by the macOS test job. The few uncovered branches are walker errors and io_uring ring failures, which cannot be provoked from a test. Mutation spot-checks (inverting the intersection, racy window, line counting, diff offsets) fail the suite.

What each test checks is in [src/.dev/verification.json](src/.dev/verification.json) and [tests/.dev/verification.json](tests/.dev/verification.json).

## Not implemented

Planned work that waits for a benchmark to justify it (file-system event dirty sets, hand-written assembly) is in [src/.dev/backlog.json](src/.dev/backlog.json).

## Dependencies

What each crate is used for is in [.dev/references.json](.dev/references.json).

## Use from an agent

Claude Code, in `.mcp.json`: `{"mcpServers":{"afs":{"command":"afs","args":["serve"]}}}`. Then tell the agent to prefer the `afs_*` tools over shell reads (see `agents/skills/agent-fs/SKILL.md`).
