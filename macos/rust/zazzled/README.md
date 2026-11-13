# zazzle / zazzled

A file tool for agents: `zazzle` replaces `grep`, `cat`, `sed -n`, `find`, `ls` and `sed -i` with commands whose output is short, capped, ranked and deduplicated per session.

- `zazzle` (`macos/c/zazzle/`): a tiny C program with no libc on Linux (raw syscalls); on macOS it links libSystem but still uses raw syscalls for all I/O. It sends its arguments, working-directory descriptor and standard streams to the daemon and exits with the daemon's status byte.
- `zazz`: the same invoker under another name, for the zazz script commands below; installed scripts are symlinks to it.
- `zazzled` (this crate): the long-lived daemon. `zazzled serve` listens on `~/.zazzle/sock`; `zazzled mcp` is an MCP stdio server in front of the same daemon (no process spawn per call); `zazzled run CMD..` runs one command in-process; `zazzled install` / `uninstall` manage the macOS LaunchAgent.

## Setup

```bash
cd macos/rust && cargo build --release -p zazzled && cd ../c/zazzle && make     # or download the release tarball
zazzled install                       # macOS: launchd starts the daemon on demand (elsewhere zazzle starts it itself)
export ZAZZLE_SESSION=$(date +%s)     # optional: ids and dedupe for CLI use
claude mcp add zazzle -- zazzled mcp     # MCP: tools zazzle_find, zazzle_read, zazzle_ls, zazzle_outline, zazzle_refs, zazzle_edit
```

Any MCP client works with `{"mcpServers":{"zazzle":{"command":"zazzled","args":["mcp"]}}}`; the working directory of the server is the root for relative paths and the only place edits may write.

## Commands

| command | output |
|---|---|
| `find PATTERN [PATH..] [-i -F -w -c -l -u] [-C N] [-g GLOB] [-m N]` | grouped by file, `N:` match and `N-` context lines, long lines clipped around the match, capped (50) with a footer saying how to narrow; ordinary files before deep files before tests |
| `read PATH[:A-B].. [--around N --ctx N --head N --tail N --budget T --anchors --full]` | header with line count and sha, then `N<TAB>text`; elides the middle beyond ~3000 tokens with the exact range to read next |
| `ls [DIR] [-d N] [-m N]` | tree with recursive file counts and line counts |
| `outline PATH.. [-d N] [-m N]` | `start-end kind name` per symbol; tree-sitter for Rust, Python, C, JS/TS/TSX up to 1 MiB; line-based for larger files, Markdown, TOML, JSON and everything else |
| `refs SYMBOL [PATH..]` | definitions first, then uses with their enclosing symbol |
| `edit PATH ...` | `--old/--new` unique replace, `--lines` with `--expect SHA` or `--at` anchors, `--after`, `--create`; atomic, CRLF-preserving, confined to the working directory |
| `status` | daemon, cache and session counters |

Any command accepts `--budget T` (caps output at about T tokens, default 8000; `read` has its own) and `--disable-session-deduping`.

## zazz scripts

Agents (and humans) can ship small programs to zazzled: a `.zazz` file is a ZIP (stored or deflate, read with built-in code) holding `zazz.json`, Rust sources (`src/`, `tests/`) or a prebuilt WASM module. Design and decisions: `ZAZZ-PLAN.md`; agent how-to: `agents/skills/zazz/SKILL.md`.

| command | does |
|---|---|
| `zazz new NAME` | a script directory to start from (manifest, `src/main.rs`, `tests/basic.rs`) |
| `zazz test DIR\|FILE [--update]` | builds to WASM and runs the tests; replies with diagnostics or a test report; registers nothing |
| `zazz pack DIR [-o FILE]` | a deterministic `.zazz` (same sources, same bytes, same id) |
| `zazz add FILE` | reads the file once, ids it by the BLAKE3 of its exact bytes, builds and tests once, registers; `cached` when known |
| `zazz call ID [JSON\|-]` / `zazz call ID OP [--params J] [--body J]` | runs `json-command` or one OpenAPI operation in a warm sandboxed worker; JSON in, JSON out |
| `zazz describe\|ls\|rm` | what a script is and may touch; the registry |
| `zazz install FILE\|ID [--as NAME]`, `zazz run NAME ARGS`, `uninstall`, `list` | commands for humans: `~/.zazzle/bin/NAME` links to the invoker; flags come from the manifest's `cli` spec |
| `zazz keygen NAME [--native] [--network ...]`, `zazz sign FILE`, `zazz trust add\|ls\|rm` | signing authorities and the machine's trust in them |
| `zazz doctor` | rustc, the `wasm32-wasip1` target, zazz-worker, the OS sandbox, paths |

- **Runtime:** Rust scripts compile (std + the embedded `zazz` SDK, built once per toolchain) to `wasm32-wasip1` and run in `zazz-worker` (wasmtime). Each call gets a fresh instance with time and memory limits; workers are kept warm per (script, directory, policy) and recycled.
- **Two sandbox layers:**
  - WASI preopens only the manifest's paths; the `zazz_host` imports carry the call, progress, cancellation and allowlisted HTTPS.
  - The OS sandbox wraps every worker, `rustc` build and test run: Landlock + seccomp on Linux, `sandbox-exec` on macOS. Unsigned scripts never run without it.
- **Signing:** a `zazz.sig` (ECDSA P-256 over a canonical digest, plus the signer's chain) is checked at add. Authorities are certificates in root-owned `/etc/zazzle/trust.d` (and the Keychain on macOS); their grants (network hosts, paths, native) narrow what the script may do. Verdicts are cached for `trust_ttl` (root-owned `/etc/zazzle/policy.json`, default 600 s) and refreshed in the background, so a call never waits on trust. Only signed and trusted scripts get network; scripts trusted for native code also get a host build and run natively under the OS sandbox, falling back to WASM when trust is withdrawn.
- **Streams:** through the invoker a script reads and writes the caller's real stdin, stdout and stderr; over MCP the output is captured into the reply.
- **Store:** `~/.zazzle/v1/zazz/` (0700) keeps compiled modules, native binaries and metadata, never the `.zazz` itself.

## Session ids and dedupe

With `ZAZZLE_SESSION` set (or over MCP, where each connection is a session) every response starts with `[r:ID]`. Repeating a request whose response is identical and was sent within the last 40,000 estimated tokens returns `[r:ID] already sent: zazzle read a.rs:1-20, unchanged. Re-run with --disable-session-deduping to resend.` Edits are never deduplicated. Without a session there are no ids and no dedupe. State lives in daemon memory only.

## Cache

`~/.zazzle/v1/` (mode 0700, excluded from Spotlight and Time Machine) holds derived data only, never file text: an append-only log of per-path records (stat identity, BLAKE3 hash, line count, binary/minified/CRLF flags) with per-record checksums, torn-tail repair and compaction. In memory: line indexes for recently read files (edit anchors are hashed per line on demand), outlines keyed by content hash, and per-root file listings validated by `stat` of each directory and ignore file (no watcher; measured below).

## Measurements

Linux x86_64 VM, p50 per process spawn over 3000 runs, microseconds (`macos/c/zazzle/bench/run.sh`): floor (`_exit`) 120, `zazzle version` 209, `zazzle read` 228 vs `cat` 1057, `zazzle find` on a small tree 554 vs `grep -rn` 1278, libc hello 941. On a 111 MB tree of 6,751 files: `find` 25-35 ms vs `grep -rn` 150 ms; `ls` with line counts 1088 ms cold, 52 ms after a daemon restart; walk share of `find` fell from 33 ms to 10 ms with the validated listing cache, so no file watcher was added. macOS (GitHub Actions virtual M1, noisy, 3000 runs, p50): floor 1734, libc hello 1662, `zazzle version` 1799, `zazzle read` 1736 vs `cat` 1938, `zazzle find` on a small tree 2361 vs `grep -rn` 2471, so `zazzle` costs about what any libSystem program costs and the gain there is in output size, not startup. On macOS the floor is dyld plus libSystem (a freestanding Mach-O is killed by the kernel), so the invoker can match but not beat a libSystem hello; the MCP front end avoids the spawn entirely.

## Safety

The daemon answers only the same uid (`SO_PEERCRED` / `getpeereid`; the invoker checks the daemon too), the socket is mode 0600, requests are size-limited, and edits refuse paths outside the caller's working directory after resolving symlinks. Reads are allowed anywhere the user can read. Hard-linked files are rewritten in place; extended attributes are not preserved by the atomic replace.

## Limits

Regex is the Rust dialect (no look-around, no backreferences). `zazzle` itself supports macOS arm64 and Linux x86_64/aarch64; on Intel Macs use `zazzled mcp` or `zazzled run`. Outlines are heuristic for languages without a grammar and for files over 1 MiB (tree-sitter would take about 1.5 s on an 8.7 MB generated parser). `find`, `refs`, `ls` and listing validation run on a thread pool started once per daemon, not on threads spawned per request. `agent-fs` (`afs`) is a separate tool in this repository with overlapping goals.

## Tests

`cargo test -p zazzled`; branch coverage (nightly, test code excluded, target 95%): `sh zazzled/coverage.sh 95` from `macos/rust` (currently 100%). C invoker: `make unit coverage smoke` in `macos/c/zazzle` (branch coverage of the invoker logic, 100%).

Dependencies, each for one reason: `blake3` (hashing), `ignore` (gitignore-aware walking), `libc` (sockets, descriptors passing, launchd), `memchr`, `regex`, `serde_json` (MCP), `tree-sitter` and four grammars (outlines).
