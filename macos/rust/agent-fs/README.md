# agent-fs (`afs`)

JSON-in, JSON-out replacement for the `cat`, `sed -n`, `grep -r`, `git grep` and `find` calls agents repeat. Most of an agent's context cost is re-reading the same output, so `afs` remembers what each session has already received and sends only what changed.

Build: `cargo build --release -p agent-fs` in `macos/rust` (Linux and macOS), or download the universal macOS binary: `https://icelander.net/downloads/toolbox/latest/toolbox-macos-universal.tar.gz` (with a `.sha256` next to it; also on the GitHub releases). Or with Homebrew: `brew tap david-brandt-the-icelander/toolbox https://github.com/david-brandt-the-icelander/software-developers-toolbox`, then `brew install david-brandt-the-icelander/toolbox/agent-fs`. A browser download of an unsigned build makes macOS say it "could not verify" the binary; run `xattr -d com.apple.quarantine afs` after unpacking.

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

## Token savers

- **Unchanged**: repeating a read or search whose answer was already sent returns `{"unchanged":true,"etag","ref"}` with no content. `fresh:true` forces the content again.
- **Diff**: re-reading a whole file that changed since this session fully received it returns `hunks` (`at` = 1-based old line, `del` = old lines removed, `ins` = new text) when smaller than the file. Apply hunks from the last `at` to the first.
- **Caps**: output is bounded; `truncated`, `next_ranges` (read) or `next_cursor` (list) and `bytes_omitted` say how to continue. Lines over 2000 bytes are cut (`long_lines_cut`). Binary files report `binary:true`.
- **Compact results**: search groups by file, merges overlapping context, and `fields:["line"]` returns bare number arrays.

## Caching

- A file's etag is the first 64 bits of its BLAKE3 hash. Stamps (device, inode, size, mtime, ctime in ns) let a repeat skip hashing; a stamp younger than 2 seconds is never trusted (the "racy" rule git uses), so same-tick edits cannot serve stale data.
- Search, list and outline results are cached with the stamp of every file and directory the walk visited, plus each directory's `.gitignore` and `.ignore`. A hit re-stats those paths (no reads, no regex work) and recomputes on any difference.
- Session memory is bounded: 64 MiB of file versions kept as diff bases (files over 8 MiB are not kept), 256 cached results.

## Outline

`outline` parses with tree-sitter and returns every item with its exact line range, so an agent can `read` just `[start, end]` instead of the file. Names inside containers (impl/trait/mod, class, namespace, interface bodies) are dotted: `A.go`, `n.K.m`; Rust `impl` items read `Trait for Type`; JS/TS `const f = () => ...` counts as a function. Function bodies are not entered, so locals and nested functions are omitted. Broken syntax still yields the items that parse. Files over 2 MiB are skipped, and languages without a grammar (kt cs swift scala sh md) use line patterns where `end` equals `start`.

## Trigram index

From the second `search` on a given set of paths/globs/flags, `afs` builds an in-memory trigram index (ASCII-case-folded 3-byte sequences, posting lists of file ids). The regex is parsed and the literals every match must start or end with are extracted (`regex-syntax`); only files containing all trigrams of at least one literal are read. Patterns with no literal of 3+ bytes (`\w+`, `a*`, `ab`) scan everything. Binary files are skipped, files over 2 MiB and files younger than the 2 s racy window are always scanned, and changed, new and deleted files are re-read or dropped on the next walk (stamps decide), so results equal a full scan (tested). The first indexed query reads each file once, indexing and scanning in the same pass. The index is dropped above ~30M posting entries. It lives in the session (`serve`/`jsonl`), not on disk.

## I/O

Whole-file reads through `read(2)` with `O_NOATIME` and `posix_fadvise(SEQUENTIAL)` on Linux; ripgrep's regex and `memchr` (runtime-detected SIMD) for matching; the `ignore` crate's parallel walker. `mmap` is deliberately not used: truncating a mapped file raises SIGBUS and would kill a long-lived server.

Search loads files in chunks of 32 through `fsio::load_many`, which has two implementations of the same contract:

- **Linux, io_uring** (`uring.rs`): per chunk, one round opens every file (`OpenAt`), one round stats and reads them (`Statx` linked before `Read` so the stamp is taken before the bytes, 64 KiB first read, `pread` for the remainder of bigger files), one round closes them. Rings are pooled and probed for the needed opcodes; any ring failure disables it for the process.
- **macOS, POSIX AIO** (`aio.rs`): files are opened and stamped one by one (macOS has no async open), then each batch of 16 is read with a single `lio_listio(LIO_WAIT)` after `F_RDAHEAD`. Empty and over-4 MiB files use plain reads. Type-checked against the Apple `libc` bindings and unit-tested in macOS CI, but not benchmarked on a Mac.
- **Choice**: batching is not always faster (on a 4-core VM with a hot page cache io_uring was about 1.6x *slower* than plain reads for 7k small files, because cached opens and reads are punted to kernel worker threads). So each process times both methods on its first 128 files each and keeps the faster. `AFS_BATCH_IO=always|never` forces it; the default is `auto`.

## Testing

`cargo test -p agent-fs` runs unit tests beside the code and `tests/cli.rs`, which drives the real binary (arguments, stdin, exit codes, `jsonl`, MCP). CI gates `agent-fs` at 95% line and 95% branch coverage on Linux (`cargo +nightly llvm-cov -p agent-fs --branch`); measured at the time of writing: 99.0% lines, 98.7% branches (294 of 298), 98 unit tests plus 9 CLI tests. The macOS-only `aio.rs` is not compiled on Linux, so it is covered only by the macOS test job. The few uncovered branches are walker errors and io_uring ring failures, which cannot be provoked from a test. Mutation spot-checks (inverting the intersection, racy window, line counting, diff offsets) fail the suite.

## Not implemented

Planned, but absent until a benchmark justifies them: inotify/fanotify/FSEvents dirty sets (staleness is detected by re-stat instead), hand-written assembly (the compiler-vectorised `memchr` already saturates memory bandwidth on the hot loops). `search` patterns are matched across the whole buffer in multi-line mode, and a match is reported on the line where it starts.

## Dependencies

`blake3` hashing; `ignore` gitignore-aware parallel walking; `regex` and `memchr` matching; `regex-syntax` literal extraction for the index; `tree-sitter` and the per-language grammar crates the outline parser (they compile C at build time); `similar` line diffs; `serde`/`serde_json` the JSON protocol; `clap` the CLI; `libc` read hints and the macOS AIO calls; `io-uring` (Linux only) the ring; `tempfile` (tests) fixture directories.

## Use from an agent

Claude Code, in `.mcp.json`: `{"mcpServers":{"afs":{"command":"afs","args":["serve"]}}}`. Then tell the agent to prefer the `afs_*` tools over shell reads (see `agents/skills/agent-fs/SKILL.md`).
