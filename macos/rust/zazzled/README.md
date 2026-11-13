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

## HTML and Razor terse view

`zazzle read` on `.html`, `.htm`, `.xhtml`, `.cshtml` and `.razor` files returns a terse view built inside `zazzled`, in the same line format and with no sha; `--source` returns the code. A file with nothing to shorten comes back as source with its sha.

- Hidden: comments of 3+ lines (`<!-- -->`, Razor `@* *@`), `<style>` and `<svg>` elements that start and end their own lines, runs of 2+ `<meta>`/`<link>`/`<base>` lines, and in Razor runs of 2+ `@using`/`@addTagHelper`/`@removeTagHelper` lines.
- Shortened: `class` values with more than 4 classes to their first 3 and `…`, `style` values over 40 characters to `…`; values containing `@`, `{{` or `${` are left alone.
- Shown as they are: `<script>`, `<pre>` and `<textarea>` blocks, and Razor `@code`, `@functions` and `@{ }` blocks (C# in them is not shortened). A comment or element that shares its first or last line with other markup is not hidden.

## Edits need the sha

Every edit except `--create` needs proof that the caller read the current code: `--expect SHA` from `read --source`, or `--at` anchors from `read --source --anchors`. A refusal never reveals the current sha or anchors (`the file is not the version you read; read it again with --source`). A successful edit returns the new sha, since the caller wrote the change.

## Symbols

`zazzle read FILE --symbol NAME` (`-s`) reads one symbol instead of the whole file: the outline gives its range, which is extended upward over the doc comments, comments and attributes directly above it (`///`, `/** */`, `#[...]`, C# `[...]`, `@decorator`, Python `#`), so a C# method's XML doc comes with it.

- `NAME` is the symbol's name or the last parts of its qualified name: `Invoice.Total`, `billing::tax::total`. Containers come from nesting (classes, impls, modules, namespaces); `impl Display for A` counts as `A`; generics are ignored. Exact case is tried first, then any case.
- Overloads are picked by parameter types: `Total(decimal, int)`, `Total()`. Types are compared as written in the declaration, ignoring spaces (`Dictionary<string,int>` matches `Dictionary<string, int>`, but `decimal` does not match `System.Decimal`); both `Type name` and `name: Type` declarations work, defaults, attributes and modifiers (`ref`, `out`, `params`, `mut`) are ignored, receivers (`self`, `&self`, Python's `self`/`cls`) are not counted, and an untyped parameter matches by name. A list that fits no overload shows the overloads there are.
- Several matches (overloads, the same method in two impls) are listed with their lines and declaration line, numbered; add the parameter types or `#N` (`Total#2`) to pick one. No match lists similar names, or points at `zazzle outline`.
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

`zazzle run -- cargo test` runs a command in the caller's environment and prints what an agent needs from its output. The invoker `exec`s `zazzled wrap ARGS` (on Intel Macs run `zazzled wrap` directly); it never runs inside the daemon ([why](src/.dev/decisions.json)).

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

- `zazzle proxy ARGS` replaces itself (`exec`) with `zazzled proxy ARGS` and does not run inside the daemon ([why](src/.dev/decisions.json)). On Intel Macs, where there is no `zazzle`, run `zazzled proxy` directly.
- What changes: the `text` of `content` items (and of embedded `resource` items) in `tools/call` results, and the `text` of `contents` in `resources/read` results. A text is replaced only when that saves at least one token.
- What never changes: `structuredContent`, images and other binary items, errors, protocol messages (`initialize`, lists, notifications), requests in either direction, and replies to anything else. Responses are matched to requests by JSON-RPC id, single or batched.
- Options: `--lossless` and `--budget T` as for `clean` (budget per text, default 8000); `--skip TOOL` (repeatable) leaves one tool's results alone; `--log` writes `tools/call NAME ~A→~B tokens` to standard error for each cleaned result; `--config FILE` sets them per tool.
- `--config FILE` is JSON. Each tool (by exact name, else the longest matching `*` pattern) and resource reads can set `lossless`, `budget` and `skip`; what a rule leaves out comes from the command line. Unknown sections, settings or types stop the proxy at startup.

  ```json
  {"tools": {"read_file": {"lossless": true, "budget": 30000}, "github_*": {"budget": 2000}, "get_secret": {"skip": true}},
   "resources": {"lossless": true}}
  ```
- stdio: the server runs as a child with stderr passed through; the proxy exits with its exit code when it closes its output.
- HTTP: only Streamable HTTP (MCP 2025-03-26 and later), not the older HTTP+SSE transport. Connections stay open for further requests unless the client asks to close them (`Connection: close`, or HTTP/1.0 without keep-alive), sends something unreadable or stays idle for 60 seconds; replies of unknown length are chunked on open connections, and the upstream side reuses its connections too. JSON replies are cleaned whole (up to 64 MiB, larger ones are passed through), `text/event-stream` replies event by event as they stream, and anything else (including compressed bodies) is copied. Headers such as `Mcp-Session-Id` and `Authorization` pass both ways; hop-by-hop headers are dropped, bodies are requested uncompressed, redirects and error statuses are returned to the client. `Origin` headers from hosts other than localhost are refused (403). It listens on `127.0.0.1:8808` unless told otherwise; request bodies need `Content-Length`. Upstream TLS uses the operating system's trust store, and `HTTPS_PROXY`/`NO_PROXY` are honoured.

## Stored outputs

`clean`, `run` and `proxy` outputs cannot be produced again (their input was a pipe, a command's output or another server's reply), so they are stored on disk under an id: `clean` and `run` replies start with `[r:ID]`, and the proxy adds an item `[r:ID shortened by zazzle]` after each text it shortened (its own item, so JSON text stays JSON). When something was cut (a budget, a profile, progress bars, blobs, long lines), the reply says `read-again r:ID --full`.

- `zazzle read-again r:ID` returns the output exactly as it was shown; `--full` returns the uncut text with its line numbers (terminal escapes removed), with `--lines A-B` and `--budget T` (default 3000).
- The store is `~/.zazzle/outputs` (`$ZAZZLE_OUTPUTS_DIR`), mode 0700 with 0600 files. Each save reaps entries older than an hour; if the store then holds more than `$ZAZZLE_OUTPUTS_MAX_GB` gigabytes (default 2), the oldest go until half the bytes remain. A failed save keeps the output and says it was not stored.

## Session ids and dedupe

With `ZAZZLE_SESSION` set (or over MCP, where each connection is a session) every response starts with `[r:ID]`. Repeating a request whose response is identical and was sent within the last 40,000 estimated tokens returns `[r:ID] already sent: zazzle read a.rs:1-20, unchanged. If you no longer have it: read-again r:ID`.

`zazzle read-again r:ID` (MCP `zazzle_read_again`) runs the request behind a session response again, in its original working directory: the same text under the same id when nothing changed, else the current version under a new id with `r:OLD changed since it was sent`. So an agent can drop a response from its context (or lose it to compaction) and get it back exactly. The session keeps the last 512 requests; `clean`, `run` and `proxy` outputs come from the output store instead (see [Stored outputs](#stored-outputs)). Responses read again never reveal more than the original request did: a terse view stays without a sha. Edits are never deduplicated. Without a session there are no ids and no dedupe. State lives in daemon memory only.

## Cache

The daemon keeps derived data only, never file text, in `~/.zazzle/v1/` (`ZAZZLE_CACHE_DIR`); design notes: [src/.dev/decisions.json](src/.dev/decisions.json), file format: [src/.dev/contracts.json](src/.dev/contracts.json).

## Measurements

Linux x86_64 VM, p50 per process spawn over 3000 runs, microseconds (`macos/c/zazzle/bench/run.sh`): floor (`_exit`) 120, `zazzle version` 209, `zazzle read` 228 vs `cat` 1057, `zazzle find` on a small tree 554 vs `grep -rn` 1278, libc hello 941. On a 111 MB tree of 6,751 files: `find` 25-35 ms vs `grep -rn` 150 ms; `ls` with line counts 1088 ms cold, 52 ms after a daemon restart; walk share of `find` fell from 33 ms to 10 ms with the validated listing cache (the decision this supports: [src/.dev/decisions.json](src/.dev/decisions.json)). macOS (GitHub Actions virtual M1, noisy, 3000 runs, p50): floor 1734, libc hello 1662, `zazzle version` 1799, `zazzle read` 1736 vs `cat` 1938, `zazzle find` on a small tree 2361 vs `grep -rn` 2471, so `zazzle` costs about what any libSystem program costs and the gain there is in output size, not startup. On macOS the floor is dyld plus libSystem ([why](../../c/zazzle/.dev/decisions.json)), so the invoker can match but not beat a libSystem hello; the MCP front end avoids the spawn entirely.

## Safety

The daemon answers only the same uid (`SO_PEERCRED` / `getpeereid`; the invoker checks the daemon too), the socket is mode 0600, requests are size-limited, and edits refuse paths outside the caller's working directory after resolving symlinks. Reads are allowed anywhere the user can read. Hard-linked files are rewritten in place; extended attributes are not preserved by the atomic replace.

## Limits

Regex is the Rust dialect (no look-around, no backreferences). `zazzle` itself supports macOS arm64 and Linux x86_64/aarch64; on Intel Macs use `zazzled mcp` or `zazzled run`. Outlines are heuristic for languages without a grammar and for files over 1 MiB (tree-sitter would take about 1.5 s on an 8.7 MB generated parser). How parallel work is run (one thread pool per daemon) and why: [src/.dev/decisions.json](src/.dev/decisions.json). `agent-fs` (`afs`) is a separate tool in this repository with overlapping goals.

## Tests

`cargo test -p zazzled`; branch coverage (nightly, test code excluded, target 95%): `sh zazzled/coverage.sh 95` from `macos/rust` (currently 100%). C invoker: `make unit coverage smoke` in `macos/c/zazzle` (branch coverage of the invoker logic, 100%).

Why each dependency: [.dev/decisions.json](.dev/decisions.json).
