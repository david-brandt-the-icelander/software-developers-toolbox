# zazzle / zazzled

A file tool for agents: `zazzle` replaces `grep`, `cat`, `sed -n`, `find`, `ls` and `sed -i` with commands whose output is short, capped, ranked and deduplicated per session.

- `zazzle` (`macos/c/zazzle/`): a tiny C program with no libc on Linux (raw syscalls); on macOS it links libSystem but still uses raw syscalls for all I/O. It sends its arguments, working-directory descriptor and standard streams to the daemon and exits with the daemon's status byte.
- `zazz`: the same invoker under another name, for the zazz script commands below; installed scripts are symlinks to it.
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
| `read PATH[:A-B].. [--source --symbol NAME[#N] --around N --ctx N --head N --tail N --budget T --anchors --full]` | header with line count and sha, then `N<TAB>text`; elides the middle beyond ~3000 tokens with the exact range to read next; `--symbol` reads one symbol with its doc comments and attributes (see [Symbols](#symbols)); C# files come as a terse view (see [C# terse view](#c-terse-view)) unless `--source` |
| `ls [DIR] [-d N] [-m N]` | tree with recursive file counts and line counts |
| `outline PATH.. [-d N] [-m N]` | `start-end kind name` per symbol; tree-sitter for Rust, Python, C, C#, JS/TS/TSX up to 1 MiB; line-based for larger files, Markdown, TOML, JSON and everything else |
| `refs SYMBOL [PATH..]` | definitions first, then uses with their enclosing symbol |
| `edit PATH ...` | `--old/--new` unique replace, `--lines` or `--after`, each with `--expect SHA` (from `read --source`) or `--at` anchors; `--create`; atomic, CRLF-preserving, confined to the working directory |
| `run [--lossless --budget T --timeout SECS --profile NAME\|none] [--] CMD ARGS..` | runs a command and prints its output cleaned with a profile, then `[exit N in Ts; …]`; exits with the command's code: see [Run](#run) |
| `proxy ...` | runs the MCP proxy as its own process: see [MCP proxy](#mcp-proxy) |
| `CMD \| clean [--as auto\|grep\|json\|text] [--lossless] [--budget T] [--stats]` | the piped output of any command, shrunk: see [Clean](#clean) |
| `status` | daemon, cache and session counters |

Any command accepts `--budget T` (caps output at about T tokens, default 8000; `read` and `clean` have their own) and `--disable-session-deduping`.

## C# terse view

`zazzle read FILE.cs` returns a terse behavioural view from `zazzle-csharp-indexer` (Roslyn) instead of the code, and no sha. `--source` (or `--anchors`) returns the code and its sha, which every edit needs.

- Hidden or collapsed: using blocks (`1-12 ⋯ 12 using(s)`), logging and tracing statements on loggers, `Debug`/`Trace` and spans with no side effects in their arguments, `if (logger.IsEnabled(..))` blocks that only log, `#region` lines, constructors that only assign fields (`13-18 ⋯ ctor(ILogger<X> logger, IRepo repo): only assigns fields`), and catch clauses that only log and `throw;`.
- Shortened: `public`/`private`/`protected`/`internal` on declarations, `var` where the type is spelled out on the right (`new T(..)`, `(T)x`, `x as T`), and local variables of 6+ characters, to the initials of their words (`invoiceRepository` → `ir`) or three letters of one word. Locals are bound by a compilation of the file, so fields, members and parameters with the same name keep theirs; a clash or keyword gets a number; a local used in `nameof` keeps its name. Parameters are never renamed.
- Kept: everything else, including catches that return or convert, constructors with logic, and logging that shares a line with other code. Lines keep their source numbers: `N` unchanged, `N~` changed, `A-B ⋯ note` hidden. Ranges, `--symbol` and the budget work on the view.
- The bridge: `zazzled` connects to the indexer daemon's socket (in `$ZAZZLE_INDEXER_STATE_DIR` or the indexer's default state directory), starting `zazzle-csharp-indexer daemon` when nothing listens, and asks its `view` tool. The indexer is `$ZAZZLE_INDEXER`, else beside `zazzled`, else on `PATH`. The file must be inside a git work tree. When there is no view (no indexer, not in git, an error), the source is returned with a line saying why. The first read starts the daemon (about 0.5 s); later ones take a few milliseconds.

## Edits need the sha

Every edit except `--create` needs proof that the caller read the current code: `--expect SHA` from `read --source`, or `--at` anchors from `read --source --anchors`. A refusal never reveals the current sha or anchors (`the file is not the version you read; read it again with --source`). A successful edit returns the new sha, since the caller wrote the change.

## Symbols

`zazzle read FILE --symbol NAME` (`-s`) reads one symbol instead of the whole file: the outline gives its range, which is extended upward over the doc comments, comments and attributes directly above it (`///`, `/** */`, `#[...]`, C# `[...]`, `@decorator`, Python `#`), so a C# method's XML doc comes with it.

- `NAME` is the symbol's name or the last parts of its qualified name: `Invoice.Total`, `billing::tax::total`. Containers come from nesting (classes, impls, modules, namespaces); `impl Display for A` counts as `A`; generics are ignored. Exact case is tried first, then any case.
- Several matches (overloads, the same method in two impls) are listed with their lines and declaration line, numbered; add `#N` (`Total#2`) to pick one. No match lists similar names, or points at `zazzle outline`.
- The usual budget applies, and `--symbol` cannot be combined with `PATH:A-B`, `--around`, `--head` or `--tail`. Over MCP, `zazzle_read` takes `symbol`.

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

## Run

`zazzle run -- cargo test` runs a command in the caller's environment and prints what an agent needs from its output. The invoker `exec`s `zazzled wrap ARGS` (on Intel Macs run `zazzled wrap` directly); it never runs inside the daemon.

- stdout and stderr go through one pipe, so their order is kept; stdin is the caller's. The exit status is the command's (128 + N for a signal); `--timeout SECS` (default 3600) kills it and exits 124. Up to 64 MiB of output is read.
- A profile, picked from the command (through `npx`, `uv run`, `python -m`, `env VAR=x`, …) or named with `--profile`, drops only lines it knows are noise and counts them. Lines it does not recognise are always kept.

| profile | commands | drops |
|---|---|---|
| cargo | `cargo …`, `cargo-nextest` | progress (`Compiling`, `Running`, `Downloaded`, …), `test … ok`, nextest `PASS`, empty test runs, std/core frames in backtraces |
| pytest | `pytest`, `python -m pytest` | session header, `PASSED` lines, progress lines with no failure |
| go | `go test`, `go vet`, `go build` | `ok` packages, `--- PASS`, `=== RUN`, packages without tests |
| dotnet | `dotnet …` | restore and `->` build lines, banners, `Passed` tests, System/Microsoft/xUnit/NUnit frames, the repeated warnings and errors MSBuild prints again at the end |
| js-test | `jest`, `vitest`, `mocha`, `npm test` and the like | `PASS` files, `✓` tests, banners |
| tsc | `tsc`, `vue-tsc` (also through `npx`) | `tsc -b --verbose` progress and the `--pretty` per-file summary table; diagnostics are grouped under their file (`src/a.ts` then `1:14 error TS2322: …`), code frames kept |
| install | `npm/pnpm/yarn/bun install`, `pip install`, `uv pip install` | download and resolve progress |

- Then `clean`'s steps and budget apply (`--lossless` turns off the profile and the lossy steps), and a footer says what happened: `[exit 101 in 8.4s; cargo profile dropped 6 progress lines, 333 passing tests]`.
- Measured: this crate's `cargo test` (334 passing tests) goes from about 5,300 tokens to about 80; 40 `tsc` errors across 4 files go from about 1,300 tokens to 850 (2,800 to 1,500 with `--pretty`).

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

With `ZAZZLE_SESSION` set (or over MCP, where each connection is a session) every response starts with `[r:ID]`. Repeating a request whose response is identical and was sent within the last 40,000 estimated tokens returns `[r:ID] already sent: zazzle read a.rs:1-20, unchanged. If you no longer have it: read-again r:ID`.

`zazzle read-again r:ID` (MCP `zazzle_read_again`) runs the request behind a response again, in its original working directory: the same text under the same id when nothing changed, else the current version under a new id with `r:OLD changed since it was sent`. So an agent can drop a response from its context (or lose it to compaction) and get it back exactly. The session keeps the last 512 requests; `clean` responses cannot be read again, since their input is gone. Responses read again never reveal more than the original request did: a terse view stays without a sha. Edits are never deduplicated. Without a session there are no ids and no dedupe. State lives in daemon memory only.

## Cache

`~/.zazzle/v1/` (mode 0700, excluded from Spotlight and Time Machine) holds derived data only, never file text: an append-only log of per-path records (stat identity, line count, binary/minified/CRLF flags; `ls` and `find` fill it without hashing files, and the record's old 32-byte hash slot is kept as zeros) with per-record checksums, torn-tail repair and compaction. In memory: line indexes for recently read files (edit anchors are hashed per line on demand), outlines keyed by content hash, and per-root file listings validated by `stat` of each directory and ignore file (no watcher; measured below).

## Measurements

Linux x86_64 VM, p50 per process spawn over 3000 runs, microseconds (`macos/c/zazzle/bench/run.sh`): floor (`_exit`) 120, `zazzle version` 209, `zazzle read` 228 vs `cat` 1057, `zazzle find` on a small tree 554 vs `grep -rn` 1278, libc hello 941. On a 111 MB tree of 6,751 files: `find` 25-35 ms vs `grep -rn` 150 ms; `ls` with line counts 1088 ms cold, 52 ms after a daemon restart; walk share of `find` fell from 33 ms to 10 ms with the validated listing cache, so no file watcher was added. macOS (GitHub Actions virtual M1, noisy, 3000 runs, p50): floor 1734, libc hello 1662, `zazzle version` 1799, `zazzle read` 1736 vs `cat` 1938, `zazzle find` on a small tree 2361 vs `grep -rn` 2471, so `zazzle` costs about what any libSystem program costs and the gain there is in output size, not startup. On macOS the floor is dyld plus libSystem (a freestanding Mach-O is killed by the kernel), so the invoker can match but not beat a libSystem hello; the MCP front end avoids the spawn entirely.

## Safety

The daemon answers only the same uid (`SO_PEERCRED` / `getpeereid`; the invoker checks the daemon too), the socket is mode 0600, requests are size-limited, and edits refuse paths outside the caller's working directory after resolving symlinks. Reads are allowed anywhere the user can read. Hard-linked files are rewritten in place; extended attributes are not preserved by the atomic replace.

## Limits

Regex is the Rust dialect (no look-around, no backreferences). `zazzle` itself supports macOS arm64 and Linux x86_64/aarch64; on Intel Macs use `zazzled mcp` or `zazzled run`. Outlines are heuristic for languages without a grammar and for files over 1 MiB (tree-sitter would take about 1.5 s on an 8.7 MB generated parser). `find`, `refs`, `ls` and listing validation run on a thread pool started once per daemon, not on threads spawned per request. `agent-fs` (`afs`) is a separate tool in this repository with overlapping goals.

## Tests

`cargo test -p zazzled`; branch coverage (nightly, test code excluded, target 95%): `sh zazzled/coverage.sh 95` from `macos/rust` (currently 100%). C invoker: `make unit coverage smoke` in `macos/c/zazzle` (branch coverage of the invoker logic, 100%).

Dependencies, each for one reason: `blake3` (hashing), `ignore` (gitignore-aware walking), `libc` (sockets, descriptors passing, launchd), `memchr`, `regex`, `serde_json` (MCP), `tree-sitter` and five grammars (outlines and `read --symbol`), `ureq` with rustls and the platform verifier (the HTTP side of the MCP proxy).
