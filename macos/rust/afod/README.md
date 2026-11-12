# afo / afod

A file tool for agents: `afo` replaces `grep`, `cat`, `sed -n`, `find`, `ls` and `sed -i` with commands whose output is short, capped, ranked and deduplicated per session.

- `afo` (`macos/c/afo/`): a tiny C program with no libc on Linux (raw syscalls); on macOS it links libSystem but still uses raw syscalls for all I/O. It sends its arguments, working-directory descriptor and standard streams to the daemon and exits with the daemon's status byte.
- `afod` (this crate): the long-lived daemon. `afod serve` listens on `~/.agent-file-operations/sock`; `afod mcp` is an MCP stdio server in front of the same daemon (no process spawn per call); `afod run CMD..` runs one command in-process; `afod install` / `uninstall` manage the macOS LaunchAgent.

## Commands

| command | output |
|---|---|
| `find PATTERN [PATH..] [-i -F -w -c -l -u] [-C N] [-g GLOB] [-m N]` | grouped by file, `N:` match and `N-` context lines, long lines clipped around the match, capped (50) with a footer saying how to narrow; ordinary files before deep files before tests |
| `read PATH[:A-B].. [--around N --ctx N --head N --tail N --budget T --anchors --full]` | header with line count and sha, then `N<TAB>text`; elides the middle beyond ~3000 tokens with the exact range to read next |
| `ls [DIR] [-d N] [-m N]` | tree with recursive file counts and line counts |
| `outline PATH.. [-d N] [-m N]` | `start-end kind name` per symbol; tree-sitter for Rust, Python, C, JS/TS/TSX; line-based for Markdown, TOML, JSON and everything else |
| `refs SYMBOL [PATH..]` | definitions first, then uses with their enclosing symbol |
| `edit PATH ...` | `--old/--new` unique replace, `--lines` with `--expect SHA` or `--at` anchors, `--after`, `--create`; atomic, CRLF-preserving, confined to the working directory |
| `status` | daemon, cache and session counters |

Any command accepts `--budget T` (caps output at about T tokens, default 8000; `read` has its own) and `--disable-session-deduping`.

## Session ids and dedupe

With `AFO_SESSION` set (or over MCP, where each connection is a session) every response starts with `[r:ID]`. Repeating a request whose response is identical and was sent within the last 40,000 estimated tokens returns `[r:ID] already sent: afo read a.rs:1-20, unchanged. Re-run with --disable-session-deduping to resend.` Edits are never deduplicated. Without a session there are no ids and no dedupe. State lives in daemon memory only.

## Cache

`~/.agent-file-operations/v1/` (mode 0700, excluded from Spotlight and Time Machine) holds derived data only, never file text: an append-only log of per-path records (stat identity, BLAKE3 hash, line count, binary/minified/CRLF flags) with per-record checksums, torn-tail repair and compaction. In memory: line indexes and edit anchors for recently read files, outlines keyed by content hash, and per-root file listings validated by `stat` of each directory and ignore file (no watcher; measured below).

## Measurements

Linux x86_64 VM, p50 per process spawn, microseconds (`macos/c/afo/bench/run.sh`): floor (`_exit`) 119, `afo version` 222, `afo read` 196 vs `cat` 1088, libc hello 1058. On a 111 MB tree of 6,751 files: `find` 25-35 ms vs `grep -rn` 150 ms; `ls` with line counts 1088 ms cold, 52 ms after a daemon restart; walk share of `find` fell from 33 ms to 10 ms with the validated listing cache, so no file watcher was added. On macOS the floor is dyld plus libSystem (a freestanding Mach-O is killed by the kernel), so the invoker can match but not beat a libSystem hello; the MCP front end avoids the spawn entirely.

## Safety

The daemon answers only the same uid (`SO_PEERCRED` / `getpeereid`; the invoker checks the daemon too), the socket is mode 0600, requests are size-limited, and edits refuse paths outside the caller's working directory after resolving symlinks. Reads are allowed anywhere the user can read. Hard-linked files are rewritten in place; extended attributes are not preserved by the atomic replace.

## Limits

Regex is the Rust dialect (no look-around, no backreferences). `afo` itself supports macOS arm64 and Linux x86_64/aarch64; on Intel Macs use `afod mcp` or `afod run`. Outlines are heuristic for languages without a grammar. `agent-fs` (`afs`) is a separate tool in this repository with overlapping goals.

## Tests

`cargo test -p afod`; branch coverage (nightly, test code excluded, target 95%): `sh afod/coverage.sh 95` from `macos/rust` (currently 100%). C invoker: `make unit coverage smoke` in `macos/c/afo` (branch coverage of the invoker logic, 100%).

Dependencies, each for one reason: `blake3` (hashing), `ignore` (gitignore-aware walking), `libc` (sockets, descriptors passing, launchd), `memchr`, `regex`, `serde_json` (MCP), `tree-sitter` and four grammars (outlines).
