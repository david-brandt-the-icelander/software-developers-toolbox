# Plan: `zazz` scripts for zazzle(d)

Status: draft, not implemented. Decisions marked **D1–D5** need the owner's answer (end of file).

## 1. What it is

Agents write a small Rust program, pack it into one compressed file (`NAME.zazz`) and give zazzled its absolute path. zazzled hashes it, compiles it once, and from then on runs it by hash, inside a sandbox, through a fixed protocol. Humans can install the same scripts as ordinary commands with parsed flags and normal stdin/stdout.

```
agent:  zazz pack ./csv-sum -o /tmp/csv-sum.zazz        # deterministic, our own deflate
        zazz add /tmp/csv-sum.zazz                       # -> {"hash":"b3:9f2c…","messages":[…]}
        zazz call 9f2c41d0a7e1 '{"files":["a.csv"]}'     # json in, json out
human:  zazz install /tmp/csv-sum.zazz                   # shim ~/.zazzle/bin/csv-sum
        csv-sum -d ';' --header a.csv | sort             # parsed flags, real pipes
```

## 2. Fixed points (from the brief)

- One file, compressed. We decompress with code compiled into zazzled; no `unzip`, `tar`, `gzip` or similar from the environment.
- A secure hash identifies a script (BLAKE3, already a dependency).
- New hash: decompress and compile. Known hash: reuse the binary we already have.
- We keep no copy of the `.zazz` file. Only the compiled binary and its metadata stay.
- Scripts run under `sandbox-exec`, using the global policy plus whatever extra limits the script asks for. A script can only narrow the policy, never widen it.
- Protocol: `json-command` (required); `openapi` + `service-call` (recommended). JSON everywhere unless stated otherwise.
- Script processes start on demand (JIT) and the launcher may recycle them at any time.
- The shell command is `zazz`; the file extension is `.zazz`.
- Human use: no JSON, args pre-parsed by the launcher and given to the script as JSON, stdio streams supported.

## 3. The `.zazz` file

- **Format:** a ZIP archive, limited to *stored* and *deflate* entries. Agents can produce it with `zazz pack` (preferred) or with any zip tool. Inflate and deflate come from `miniz_oxide` (pure Rust, one new dependency). CRC-32 is a 20-line table in our code.
- **Limits** (constants; violations are rejected before any extraction):
  - at most 8 MiB compressed, 32 MiB uncompressed in total, 256 entries, 4 MiB per entry, and a compression ratio of at most 100:1;
  - no zip64, encryption, multi-disk archives, symlink entries (external attributes) or data descriptors that disagree with the central directory.
- **Names:** relative and UTF-8, with `/` as the only separator. Rejected: `..`, empty or `.` components, leading `/`, backslashes, NUL, and names that collide after case folding (APFS is case-insensitive).
- **Contents:**
  ```
  zazz.json          manifest (required)
  src/main.rs        entry point (path set in the manifest)
  src/**.rs          further modules
  ```
  `build.rs`, `Cargo.toml` dependencies and proc-macros are not accepted in v1 (see D1).
- **`zazz pack DIR -o FILE`:** sorted entries, fixed timestamps (1980-01-01), fixed attributes and deflate level 9, so the same sources always give the same bytes and the same hash.

### Manifest `zazz.json`

```json
{
  "zazz": 1,
  "name": "csv-sum",
  "version": "0.1.0",
  "description": "Sum numeric columns of CSV files.",
  "entry": "src/main.rs",
  "edition": "2021",
  "messages": ["json-command", "openapi", "service-call"],
  "sandbox": { "read": ["$CWD"], "write": [], "network": "none", "exec": false },
  "limits": { "call_ms": 30000, "memory_mb": 512 },
  "cli": {
    "usage": "csv-sum [-d DELIM] [--header] FILE...",
    "stdin": "optional",
    "args": [
      { "name": "delim",  "short": "d", "long": "delimiter", "type": "string", "default": "," },
      { "name": "header", "long": "header", "type": "bool" },
      { "name": "files",  "positional": true, "type": "path", "multiple": true }
    ]
  }
}
```

## 4. Hashing, locking, compiling (`zazz add PATH`)

1. Open `PATH` with `O_RDONLY|O_NOFOLLOW|O_CLOEXEC`. The caller's uid must already be able to read it (we check the open, not `access()`). Require a regular file no larger than 8 MiB. Take a shared `flock` on it while reading so cooperative writers wait.
2. Read the whole file into memory **once**, and do all later work on those bytes, never on the path again. This rules out swap-after-hash (TOCTOU) races.
3. `hash = BLAKE3(bytes)`; the id is `b3:<64 hex>`. Commands accept any unique prefix of at least 12 hex characters.
4. If `bin/<hash>` and `meta/<hash>.json` exist and the build key matches, reply `cached`. Done.
5. Otherwise take an exclusive `flock` on `locks/<hash>.lock`, which serialises the daemon, `zazzled run` and parallel callers. After getting the lock, check step 4 again.
6. Validate the archive, then extract it from memory into a new private directory `tmp/<hash>.<pid>/` (mode 0700, every file opened with `O_CREAT|O_EXCL|O_NOFOLLOW`).
7. Compile (§5) **inside the compile sandbox**.
8. Ask the new binary for `openapi` (if it declares it) and cache the reply in its metadata.
9. Publish atomically: write `meta/<hash>.json.tmp` and rename it; rename the binary to `bin/<hash>`.
10. Delete `tmp/<hash>.<pid>/` (sources) and release the lock. On any failure, delete the temp directory too; nothing half-built is ever visible.

**Build key** = BLAKE3 of (rustc `-vV` output, SDK version, target triple, profile flags). If the toolchain changes, existing binaries keep working. `zazz ls` marks them `stale` but they are not rebuilt, because we no longer have the sources: an agent re-adds the file when it wants a rebuild.

**Store** `~/.zazzle/v1/zazz/` (mode 0700, already excluded from Spotlight and Time Machine):

```
bin/<hash>            compiled executable (0500)
meta/<hash>.json      manifest, build key, openapi doc, added/last-used times, source path hint
names/<name>          -> hash, for humans (one small file per name)
locks/<hash>.lock     compile lock
tmp/                  build directories; anything older than 1 h is removed at startup
sdk/<buildkey>/       prebuilt libzazz.rlib per toolchain
```

`zazz rm HASH`, `zazz gc [--keep-days N]` (LRU on last use; never removes names that are installed).

## 5. Compilation

- **Toolchain:** `rustc` is the one tool we need from the environment, and it is not optional. Look it up deterministically: `$ZAZZ_RUSTC`, then `~/.cargo/bin/rustc`, then `/opt/homebrew/bin/rustc`. Do not search `PATH`, because the LaunchAgent starts zazzled with an empty one. On macOS the linker is resolved **once** to an absolute clang path (via `xcrun -f clang`) and cached with the build key, so builds do not run `xcrun` each time.
- **Command:** call `rustc` directly, not cargo: `rustc --edition E --crate-type bin --crate-name zazz_<short> -C opt-level=2 -C panic=unwind -C strip=symbols --error-format=json --extern zazz=sdk/<key>/libzazz.rlib -o tmp/…/out src/main.rs`. Run it with an argument list, a 120 s timeout and the environment cleared.
- **SDK (`zazz` crate):** a std-only crate in the workspace (`macos/rust/zazz-sdk/`), tested and linted like the others. zazzled embeds its source with `include_str!` and compiles it to an rlib the first time it is needed for each build key.
- **Diagnostics:** parse rustc's JSON output into a short list for agents: `{"level":"error","file":"src/main.rs","line":12,"col":5,"msg":"…","hint":"…"}`. At most 20 entries; every entry must fit in a reply budget.
- **Compile sandbox:** `rustc` and the linker can read only the toolchain, the SDK rlib, the build directory and the system SDK, can write only the build directory, and have no network. This blocks exfiltration through `include_str!("/Users/…/.ssh/id_ed25519")`.

## 6. Running: launcher and workers

### Process model

- **Pool key** = (hash, instantiated policy). The policy is path-based and fixed when the process starts, so a worker sandboxed for cwd `/a` never serves `/b`.
- A worker is spawned when a call has no idle worker for its key (JIT). It runs one call at a time. Up to `max_workers_per_key` (default 2) run in parallel; extra calls queue.
- **Recycling** (at our discretion; scripts must be stateless across calls): idle for more than 60 s, more than 1000 calls, RSS over the limit, any protocol error, a call timeout, or more than 32 warm workers in total (least recently used is evicted). Shutdown is `{"t":"shutdown"}`, then SIGKILL after 200 ms.
- **Spawn:** `/usr/bin/sandbox-exec -p PROFILE -D CWD=… -D TMP=… bin/<hash>` (absolute path, so no `PATH` lookup). The process gets:
  - the caller's cwd as its own;
  - fds 0/1/2 on `/dev/null`;
  - **fd 3** as one end of a `socketpair` (the control channel);
  - an environment with only `LANG`, `TMPDIR=<per-worker tmp>`, `HOME=<per-worker tmp>` and `ZAZZ_PROTO=1`;
  - before exec: `setsid`, `RLIMIT_CORE=0`, `RLIMIT_NOFILE=256`, `RLIMIT_FSIZE`.
- **Daemon threads:** a zazz call can run for minutes (human streams), so zazzled must hand it to a dedicated call thread and **never** run it on the 2–8 accept workers that serve `find`/`read`. Idle-exit already waits for active connections; when zazzled exits, it shuts down every warm worker. If zazzled dies, workers see EOF on fd 3 and exit.
- **Cancellation:** while a call runs, the call thread polls the client socket for `POLLHUP` (the invoker died, e.g. on Ctrl-C). It then sends `{"t":"cancel","id":N}` and kills the worker after 200 ms.

### Control-channel protocol (fd 3)

Frames are `u32` little-endian length + UTF-8 JSON, at most 16 MiB. Descriptors travel as `SCM_RIGHTS` on the same `sendmsg`, reusing the code in `server.rs`, which moves to `fdpass.rs`.

| direction | message | notes |
|---|---|---|
| L→S | `{"t":"hello","proto":1}` | first frame |
| S→L | `{"t":"ready","messages":["json-command","openapi","service-call"],"sdk":"1.0.0"}` | must match the manifest |
| L→S | `{"t":"json-command","id":7,"mode":"agent"\|"human","input":{…},"stdio":[0,1,2],"deadline_ms":30000}` + 3 fds | **required** message |
| L→S | `{"t":"openapi","id":1}` | returns an OpenAPI 3.1 document |
| L→S | `{"t":"service-call","id":8,"operation":"sumColumns","params":{…},"body":{…}}` + 3 fds | `operation` is an `operationId` from the document |
| S→L | `{"t":"result","id":7,"ok":true,"output":{…},"exit":0}` | `exit` is used in human mode |
| S→L | `{"t":"result","id":7,"ok":false,"error":{"code":"bad_input","message":"…"}}` | codes: `bad_input`, `not_found`, `failed`, `unsupported` |
| S→L | `{"t":"progress","id":7,"msg":"…"}` | optional; shown on stderr in human mode |
| L→S | `{"t":"cancel","id":7}` / `{"t":"shutdown"}` | |

- **stdio:** the invoker already passes its fds 0/1/2 to zazzled. zazzled forwards them to the worker with every call, so the script reads and writes the caller's **real** pipes and TTY: no copying, `isatty` works, and back-pressure works. For agent calls the final JSON `output` is written to the caller's stdout by zazzled after the result frame. Scripts must use stderr for anything else in agent mode.
- **MCP has no fds.** zazzled makes pipes, sends the tool's `stdin` string, captures stdout/stderr up to the reply budget and returns them next to `output`.
- **OpenAPI:** fetched once at `add` time and cached. `zazz describe HASH` prints a compact summary: each `operationId` with its input and output keys. The launcher does not validate against the schema in v1. The SDK offers typed helpers so scripts validate their own input.
- **Errors the launcher adds:** `script_crashed` (with the signal), `timeout`, `sandbox_denied` (reported when the worker's stderr shows `deny(` lines), `protocol` and `busy`.

### SDK sketch (what an agent writes)

```rust
// Sums numeric CSV columns.
use zazz::{json, Call, Json, Result};

// Handles one json-command call.
fn run(call: &mut Call, input: Json) -> Result<Json> {
    let delim = input.str_or("delim", ",");
    let mut totals = Vec::new();
    for path in input.paths("files")? {           // "-" means stdin
        let text = call.read_input(&path)?;
        totals.push(sum(&text, delim)?);
    }
    if call.is_human() { call.println(&format!("{totals:?}"))?; return Ok(Json::Null); }
    Ok(json!({ "totals": totals }))
}

// Serves calls until the launcher recycles this process.
fn main() { zazz::serve(zazz::Handlers::new().json_command(run)) }
```

The SDK handles the handshake, framing, receiving fds, `catch_unwind` around each call (a panic becomes `failed` and the worker exits), its own small `Json` type and parser (no serde in v1, see D1) and `Call::{stdin, stdout, stderr, read_input, println, progress, is_human, cancelled}`.

## 7. Sandbox policy

- **Capability model, not raw SBPL.** Scripts and the global policy speak only:
  ```json
  { "read": ["$CWD", "/usr/share/dict"], "write": ["$TMP"], "network": "none"|"localhost"|"all", "exec": false }
  ```
  `$CWD` (the caller's cwd), `$TMP` (per-worker scratch) and `$HOME` are placeholders. `$HOME` is denied unless the global policy grants it.
- **Global policy:** `~/.zazzle/zazz-policy.json`, which must be owned by the user, mode 0600 and not a symlink; if it is wrong it is ignored, with a warning. Built-in default: read `$CWD`; write `$TMP`; network `none`; no exec or fork; system libraries and dyld paths readable; `mach-lookup` limited to the services libSystem needs at start-up (to be measured).
- **Effective policy** = global ∩ script request. If a script asks for more than the global policy allows, `add` succeeds but reports `"runnable": false, "reason": "requests network=all; global allows none"`, and `call` refuses with the same reason. Every launch checks again, since the global policy can change.
- **macOS backend:** generate SBPL from a fixed template in which every path arrives as a `-D` parameter (`(subpath (param "CWD"))`), never by interpolating strings, so a crafted path cannot inject SBPL. `sandbox-exec` is deprecated but shipped with the OS at a fixed path, so it is not per-installation flakiness. Its removal is a tracked risk; the fallback is calling `sandbox_init_with_parameters` from libSystem ourselves.
- **Linux backend** (D3): Landlock for filesystem rules, a seccomp filter that denies `socket`/`connect` for `network:none`, `PR_SET_NO_NEW_PRIVS`, and an unshared network namespace when permitted. If no sandbox is available, refuse to run; never fall back to running unsandboxed.
- **Network granularity:** SBPL cannot filter by host name. v1 offers `none`/`localhost`/`all`; per-host allowlists come later through a zazzled HTTP proxy on localhost (D4).

## 8. The `zazz` command

**Invoker:** the existing C invoker binary, dispatched on `basename(argv[0])`:
- `zazzle` behaves as today;
- `zazz` prepends `zazz` to the args;
- any other name (an installed shim, which is a symlink to `zazz`) prepends `zazz exec-name <argv0>`.

The release ships a `zazz -> zazzle` symlink, and the Homebrew formula does the same. The fds 0/1/2 + cwd fd + status byte path stays unchanged. zazz requests are never session-deduped.

### Agent subcommands (JSON replies, short)

| command | reply |
|---|---|
| `zazz pack DIR [-o FILE]` | `{"path":…,"hash":…,"bytes":N}`; validates the manifest and limits locally |
| `zazz add PATH` | `{"hash":…,"status":"compiled"\|"cached","runnable":true,"messages":[…]}` or `{"error":{"code":"compile_failed","diagnostics":[…]}}` |
| `zazz call HASH [JSON\|-]` | the `json-command` output; input from the argument or stdin (`-`); `--timeout MS` |
| `zazz call HASH OPERATION [--params JSON] [--body JSON\|-]` | the `service-call` output |
| `zazz describe HASH` | manifest summary + operation list |
| `zazz ls` / `zazz rm HASH` / `zazz gc` | registry management |
| `zazz status` | warm workers per key, calls, recycles |
| `zazz doctor` | toolchain, linker, SDK rlib, sandbox backend: each `ok` or the exact fix |
| `zazz new NAME` | writes a ready-to-pack template directory (manifest + `main.rs`) |

**MCP:** new tools `zazz_add`, `zazz_call`, `zazz_describe` and `zazz_ls` in `mcp.rs`, using the existing tool table. A later phase can also publish each installed script's OpenAPI operations as MCP tools of their own (`notifications/tools/list_changed`).

### Human subcommands (no JSON)

- `zazz install PATH|HASH [--as NAME]`: runs `add`, writes `names/NAME` and makes the shim `~/.zazzle/bin/NAME -> zazz`. It prints one line with the `PATH` export to add. Names cannot shadow zazz subcommands, existing shims or names with `/`.
- `zazz uninstall NAME`, `zazz list`, `zazz run NAME ARGS…` (works without shims).
- `NAME ARGS…` / `zazz run NAME ARGS…`:
  1. The launcher parses `ARGS` against `cli.args`. Types: `bool`, `count` (`-vvv`), `string`, `int`, `float`, `enum`, `path` (made absolute against the caller's cwd; `-` stays `-`), with `multiple`, `default` and `required`. It also supports `--` and combined short flags (`-hv`). `--help` is generated from `usage`, the args and the description. A parse error goes to stderr with exit 2, in the usual CLI style.
  2. The result is sent as `json-command` `input` with `"mode":"human"` and the caller's real fds.
  3. The script prints whatever it likes. If it returns a non-null `output` anyway, the launcher renders it: a string is printed raw; an array of flat objects becomes a table on a TTY and TSV otherwise; anything else becomes indented JSON. `--json` forces raw JSON, and `exit` becomes the exit code.
  4. Without a `cli` section, flags are derived from the `json-command` input schema in the OpenAPI document (property → `--prop`, boolean → flag, array → repeatable). With neither, the script receives `{"argv":[…]}`.
- **Signals:** Ctrl-C kills the invoker; zazzled sees `POLLHUP` and sends `cancel`, then SIGKILL. The exit code is 130.

## 9. Security checklist

- The peer-uid check and the 0600 socket stay as they are; scripts run as the user, so the sandbox is the security boundary and must never be skipped.
- The archive is treated as hostile: limits are checked before allocating, names before extracting, and every entry is extracted with `O_EXCL|O_NOFOLLOW` into a fresh directory.
- Hash first, then work only on the in-memory bytes; we never re-open the path.
- Compilation is sandboxed, with no `build.rs` or proc-macros in v1, so no script code runs before the run sandbox exists.
- SBPL is built from parameters only; the environment is scrubbed; no fd above 3 is inherited (`O_CLOEXEC` everywhere, with an explicit close-range in `pre_exec`).
- Install names are validated, and shims only ever point at `zazz`.
- Untrusted reply text (script output, diagnostics) is budget-capped before it reaches an agent.

## 10. Code layout

```
macos/rust/zazz-sdk/            std-only SDK crate (tests + examples) - embedded in zazzled
macos/rust/zazzled/src/fdpass.rs   SCM_RIGHTS send/recv (moved out of server.rs)
macos/rust/zazzled/src/zazz/
  mod.rs        subcommand dispatch, replies
  archive.rs    zip read (limits, names) + deterministic write (pack)
  manifest.rs   zazz.json parsing and validation
  registry.rs   store layout, locks, atomic publish, names, gc
  compile.rs    toolchain discovery, SDK rlib, rustc call, diagnostics
  policy.rs     capability model, global ∩ request, placeholders
  sbpl.rs       macOS profile template + -D parameters
  landlock.rs   Linux backend (cfg target_os = "linux")
  wire.rs       frames over fd 3
  pool.rs       workers, spawn, recycle, cancel
  cliargs.rs    human arg spec parsing, --help
  render.rs     human rendering of output values
macos/c/zazzle/src/zazzle_core.h   argv[0] dispatch (+ unit tests, keep 100% branches)
agents/skills/zazz/SKILL.md     how agents write, pack, add and call scripts
```

New dependency: `miniz_oxide` (inflate/deflate). Everything else uses what is already there (`blake3`, `libc`, `serde_json`).

## 11. Phases (each one ships green: fmt, clippy, tests, branch coverage ≥ 95%)

1. **Archive + manifest + `pack`:** pure code with a large test set: traversal, zip bombs, duplicates, case collisions, symlinks, zip64, bad CRC, and determinism (same input, same hash).
2. **SDK crate:** framing, Json, handlers, `catch_unwind`; tested in-process over a socketpair.
3. **Registry + compile:** toolchain discovery, SDK rlib, rustc diagnostics, locks and atomic publish. Integration test: add a hello script twice (`compiled` then `cached`), concurrent adds compile once, a broken script returns structured diagnostics.
4. **Pool + wire, unsandboxed in tests only:** spawn, call, timeout, crash, recycle, cancel on HUP, fd forwarding (pipe in → pipe out), and the dedicated call thread so `find` is never blocked.
5. **Sandbox:** policy model and intersection (pure, tested everywhere), the SBPL generator (pure), and the macOS backend. Tests: read outside `$CWD` is denied, network is denied, fork is denied, stdio still works. Compile sandbox. Linux backend if D3 says yes.
6. **Agent front ends:** `zazz` argv[0] dispatch in the C invoker, all agent subcommands, MCP tools, `doctor`, `new`.
7. **Humans:** install/shims/names, `cliargs`, `--help`, the renderer, exit codes, Ctrl-C.
8. **`openapi` / `service-call`:** caching at add time, `describe`, CLI derived from the schema.
9. **Docs + release:** README section, `agents/skills/zazz/SKILL.md`, an AGENTS.md skills line, the release tarball and formula ship the `zazz` symlink, smoke test.

## 12. Risks and spikes (do first, an hour each)

- **SCM_RIGHTS in a `(deny default)` sandbox:** check on macOS 26 that a worker can still `recvmsg` fds on the inherited socketpair and write to a received TTY or pipe. If not, find the minimal allow rule.
- **Minimal `mach-lookup` set:** measure what a std-only Rust binary needs at start-up under `(deny default)`.
- **Cold start:** measure `sandbox-exec` + exec + handshake on macOS (expect a few ms). Warm calls should cost roughly one socket round trip.
- **macOS linker:** `rustc` needs the Command Line Tools to link (the repo has already hit CLT drift with Homebrew). `doctor` must say so plainly; there is no way around it without shipping Apple SDK stubs.
- **`sandbox-exec` deprecation:** keep the backend behind `policy.rs` so a `sandbox_init` FFI backend can replace it.
- **Coverage:** the macOS sandbox paths are only measured on macOS CI; keep the generator pure so Linux coverage still reaches the logic.

## 13. Decisions needed

- **D1 Script dependencies.** Recommended: std + `zazz` SDK only, with a tiny built-in JSON. Alternatives: a curated set vendored into zazzled (serde/serde_json etc., built once per toolchain), or arbitrary crates.io with a lockfile. The last means network access at compile time and code running at build time.
- **D2 "Lock it atomically" meaning.** Recommended: read the file once into memory (shared `flock` while reading), hash those bytes, hold an exclusive per-hash lock while compiling, then delete the extracted sources. The alternative is to keep a lock on the original file for the whole compile.
- **D3 Linux.** Recommended: run scripts on Linux with Landlock + seccomp and refuse when unavailable. The alternative is macOS-only (`add` and `call` refuse on Linux).
- **D4 Network.** v1 `none`/`localhost`/`all`; a per-host allowlist through a localhost proxy later. Or never allow network at all.
- **D5 Hash input.** Recommended: BLAKE3 of the exact `.zazz` bytes (stable thanks to deterministic `pack`). The alternative is a hash of the canonical unpacked contents, so that different zippers give the same id.
