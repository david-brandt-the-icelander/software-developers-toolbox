# zazzle / zazzled

A file tool for agents: `zazzle` replaces `grep`, `cat`, `sed -n`, `find`, `ls` and `sed -i` with commands whose output is short, capped, ranked and deduplicated per session.

- `zazzle` (`macos/c/zazzle/`): a tiny C program with no libc on Linux (raw syscalls); on macOS it links libSystem but still uses raw syscalls for all I/O. It sends its arguments, working-directory descriptor and standard streams to the daemon and exits with the daemon's status byte.
- `zazzled` (this crate): the long-lived daemon. `zazzled serve` listens on `~/.zazzle/sock`; `zazzled mcp` is an MCP stdio server in front of the same daemon (no process spawn per call); `zazzled run CMD..` runs one command in-process; `zazzled proxy` is the MCP proxy that `zazzle proxy` execs ([MCP proxy](#mcp-proxy)); `zazzled install` / `uninstall` manage the macOS LaunchAgent.

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
| `proxy ...` | runs the MCP proxy as its own process: see [MCP proxy](#mcp-proxy) |
| `CMD \| clean [--as auto\|grep\|json\|text] [--lossless] [--budget T] [--stats]` | the piped output of any command, shrunk: see [Clean](#clean) |
| `status` | daemon, cache and session counters |

Any command accepts `--budget T` (caps output at about T tokens, default 8000; `read` and `clean` have their own) and `--disable-session-deduping`.

## Clean

`grep -rn foo src | zazzle clean` (or `cargo test 2>&1 | zazzle clean`) reads standard input, up to 64 MiB, and prints what an agent needs from it:

| step | what it does | `--lossless` |
|---|---|---|
| ansi | removes colours, cursor moves and window titles | yes |
| whitespace | keeps the last frame of `\r` redraws, trims line ends, squeezes blank runs | yes |
| json | a JSON document or JSON Lines loses whitespace outside strings; keys and numbers stay as written | yes |
| grep | `path:N:text` / `path-N-text` / `path:text` output (grep, git grep, rg) is regrouped by file in `find`'s layout, only if every line parses and it is shorter | yes |
| repeats | runs of identical lines become one line and `… previous line repeated N more times` | yes |
| progress | a run of progress-bar lines becomes its last line and `… N progress lines dropped` | no |
| blobs | base64 or hex runs of 200+ characters become `<blob N chars>` | no |
| long-lines | lines over 600 bytes keep 300 bytes and `[line was N bytes]` (not for JSON) | no |
| budget | above `--budget` (default 8000 tokens) the head and tail stay and the middle becomes one line saying how much was cut | always |

Every removal leaves a marker. Input that is not recognised only gets the lossless steps, and if the markers would make the text longer, the input is passed through. `--as` skips detection (`text` turns off the json and grep steps). `--stats` adds `[clean ~A→~B tokens; steps: …]`. Repeating the same cleaned output in a session gets the usual `already sent` notice. `clean` is not an MCP tool: an MCP client already holds the text. To clean what another MCP server returns, use the [MCP proxy](#mcp-proxy).

## MCP proxy

`zazzle proxy` sits between an MCP client and another MCP server and runs `clean`'s steps over the text the server sends back:

```bash
claude mcp add fs -- zazzle proxy -- npx -y @modelcontextprotocol/server-filesystem ~/src      # stdio server
zazzle proxy --upstream https://mcp.example.com/mcp --listen 127.0.0.1:8808                      # Streamable HTTP server
claude mcp add --transport http example http://127.0.0.1:8808/mcp
```

- `zazzle proxy ARGS` replaces itself (`exec`) with `zazzled proxy ARGS`, so the proxy keeps the caller's environment (servers often read keys from it), streams and exit status; it does not run inside the daemon. On Intel Macs, where there is no `zazzle`, run `zazzled proxy` directly.
- What changes: the `text` of `content` items (and of embedded `resource` items) in `tools/call` results, and the `text` of `contents` in `resources/read` results. A text is replaced only when that saves at least one token.
- What never changes: `structuredContent`, images and other binary items, errors, protocol messages (`initialize`, lists, notifications), requests in either direction, and replies to anything else. Responses are matched to requests by JSON-RPC id, single or batched.
- Options: `--lossless` and `--budget T` as for `clean` (budget per text, default 8000); `--skip TOOL` (repeatable) leaves one tool's results alone; `--log` writes `tools/call NAME ~A→~B tokens` to standard error for each cleaned result.
- stdio: the server runs as a child with stderr passed through; the proxy exits with its exit code when it closes its output.
- HTTP: one request per connection. JSON replies are cleaned whole (up to 64 MiB, larger ones are passed through), `text/event-stream` replies event by event as they stream, and anything else (including compressed bodies) is copied. Headers such as `Mcp-Session-Id` and `Authorization` pass both ways; hop-by-hop headers are dropped, bodies are requested uncompressed, redirects and error statuses are returned to the client. `Origin` headers from hosts other than localhost are refused (403). It listens on `127.0.0.1:8808` unless told otherwise; request bodies need `Content-Length`. Upstream TLS uses the operating system's trust store, and `HTTPS_PROXY`/`NO_PROXY` are honoured.

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

Dependencies, each for one reason: `blake3` (hashing), `ignore` (gitignore-aware walking), `libc` (sockets, descriptors passing, launchd), `memchr`, `regex`, `serde_json` (MCP), `tree-sitter` and four grammars (outlines), `ureq` with rustls and the platform verifier (the HTTP side of the MCP proxy).
