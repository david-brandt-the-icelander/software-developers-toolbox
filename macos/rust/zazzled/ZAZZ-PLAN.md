# Plan: `zazz` scripts for zazzle(d)

Status: draft, not implemented. Revision 2: scripts run as WASM by default; native Rust only when signed by an accepted authority. Open decisions are D6–D8 at the end.

## 1. What it is

Agents write a small program, pack it into one compressed file (`NAME.zazz`) and give zazzled its absolute path. zazzled hashes it, compiles it once, and from then on runs it by hash through a fixed JSON protocol.

- **Default:** the script runs as **WebAssembly** inside a worker process that is itself OS-sandboxed (`sandbox-exec` on macOS, Landlock + seccomp on Linux). That gives two layers of access control: WASI capabilities and the OS sandbox.
- **Exception:** a script **signed by an accepted authority** may run as **native Rust**, limited only by the OS sandbox.

Humans can install the same scripts as ordinary commands with parsed flags and normal stdin/stdout.

```
agent:  zazz pack ./csv-sum -o /tmp/csv-sum.zazz         # deterministic, our own deflate
        zazz add /tmp/csv-sum.zazz                        # -> {"hash":"b3:9f2c…","runtime":"wasm",…}
        zazz call 9f2c41d0a7e1 '{"files":["a.csv"]}'      # json in, json out
signer: zazz pack ./gh-sync --sign ~/.zazzle/keys/me.key  # signature embedded in the .zazz
human:  zazz install /tmp/csv-sum.zazz                    # shim ~/.zazzle/bin/csv-sum
        csv-sum -d ';' --header a.csv | sort              # parsed flags, real pipes
```

## 2. Settled decisions

- **D1 WASM in place of Rust source:** a `.zazz` may carry a prebuilt `.wasm` built from any language and any WASM-compatible crates. It may also carry Rust source, which we compile ourselves (std + SDK only).
- **D2 What we read is what we compile:**
  - The file is read once into memory.
  - The hash, the signature check, extraction and compilation all use those same bytes. The path is never opened again.
  - So the output always comes from the bytes we hashed, whatever happens to the file meanwhile.
- **D3 Linux:** unsigned scripts need the full OS sandbox (Landlock + seccomp) and are refused without it. Signed native scripts bypass that rule and run with whatever sandboxing the OS offers.
- **D4 Network:** a script gets network access only when its manifest asks for it **and** it is signed by an accepted authority. An authority may be limited to certain hosts and paths. Those limits are enforced by the WASM layer; they do not apply to native Rust, because the OS sandbox cannot filter by host.
- **D5 Script id:** BLAKE3 of the exact `.zazz` bytes.
- Earlier fixed points still hold:
  - one compressed file, decompressed with our own built-in code;
  - no copy of the `.zazz` is kept;
  - script processes start on demand and are recycled at our discretion;
  - the protocol is `json-command` (required) plus `openapi`/`service-call` (recommended);
  - the shell command is `zazz`; the file extension is `.zazz`.

## 3. Execution tiers

| | unsigned (or untrusted signer) | signed, accepted, WASM | signed, accepted for `native`, Rust |
|---|---|---|---|
| runtime | WASM (wasmtime) in `zazz-worker` | WASM (wasmtime) in `zazz-worker` | native binary |
| access-control layers | WASI + OS | WASI + OS | OS only |
| network | never | manifest hosts ∩ authority hosts, enforced per request by the WASM host | on/off at OS level if the manifest asks and the authority grants any network; host list **not** enforced |
| filesystem | global ∩ manifest | global ∩ manifest ∩ authority | global ∩ manifest (authority fs limits not applied) |
| Linux without Landlock | refuse | refuse | run, with a warning in `doctor`/`status` |
| source forms | `.wasm`, or Rust compiled to `wasm32-wasip1` | same | Rust compiled natively |

- A Rust script that asks for `"native": true` but is not signed by an authority trusted for native code is **compiled to WASM instead**; the same SDK source builds both ways. `add` reports `"runtime":"wasm","native_denied":"<reason>"`.
- Trust is checked again **at every launch**, not only at `add`. A revoked or expired signer means:
  - native scripts are refused (`untrusted_signer`; re-add to get a WASM build);
  - WASM scripts drop to the unsigned tier (no network).
- Giving an authority `native` trusts it with everything the OS sandbox allows, including any host once it asks for network. `zazz trust add` says so.

## 4. The `.zazz` file

- **Format:** ZIP, limited to *stored* and *deflate* entries. Inflate and deflate come from `miniz_oxide`, built into zazzled; CRC-32 is our own table. Agents can create the file with `zazz pack` (preferred, deterministic: sorted entries, fixed timestamps, deflate level 9) or with any zip tool.
- **Limits**, checked before anything is extracted:
  - at most 32 MiB compressed, 128 MiB uncompressed, 512 entries, 64 MiB per entry, and a compression ratio of at most 100:1;
  - no zip64, encryption, multi-disk archives or symlink entries;
  - names must be relative UTF-8 with `/` only; no `..`, empty or `.` components, backslashes or NUL, and no two names that are equal after case folding.
- **Layout:**
  ```
  zazz.json        manifest (required)
  zazz.sig         signature (optional, §5)
  src/**.rs        kind "rust": sources, entry named in the manifest
  NAME.wasm        kind "wasm": one core module (wasip1/wasip2 command) or component
  ```

### Manifest `zazz.json`

```json
{
  "zazz": 1,
  "name": "gh-sync",
  "version": "0.1.0",
  "description": "Sync issue labels from a JSON file.",
  "kind": "wasm",
  "entry": "gh-sync.wasm",
  "native": false,
  "messages": ["json-command", "openapi", "service-call"],
  "sandbox": {
    "read": ["$CWD"],
    "write": [],
    "network": { "hosts": ["api.github.com"] },
    "exec": false
  },
  "limits": { "call_ms": 30000, "memory_mb": 256 },
  "cli": {
    "usage": "gh-sync [--dry-run] FILE",
    "args": [
      { "name": "dry_run", "long": "dry-run", "type": "bool" },
      { "name": "file", "positional": true, "type": "path" }
    ]
  }
}
```

- `kind` is `"wasm"` or `"rust"`. `native` is meaningful only for `"rust"`.
- `network` is `"none"` (the default), `{"hosts": [...]}` or `"all"`.

## 5. Signing and trust

- **Signature:**
  - `zazz.sig` is JSON: `{"alg":"ed25519","key":"ed25519:<base64 pubkey>","digest":"b3:<hex>","sig":"<base64>"}`.
  - `digest` is the BLAKE3 hash of every *other* entry, encoded canonically: sorted by name, then for each entry the name, the length as `u64` LE and the bytes.
  - The signature is over the `digest`, prefixed with the domain string `zazz-sig-v1\0`.
  - Verification uses `ed25519-dalek`, built in, on the in-memory bytes (D2).
- **Hash vs signature:** the script id (D5) is the hash of the whole file, including `zazz.sig`. Signing changes the file's id, never its content digest.
- **Keys:**
  - `zazz keygen NAME` writes `~/.zazzle/keys/NAME.key` (0600) and prints the public key.
  - `zazz pack DIR --sign KEY` or `zazz sign FILE --key KEY` signs.
- **Trust store** (D6): `/etc/zazzle/trust.json`, which must be owned by root and not writable by group or others; otherwise it is ignored with a warning. Root ownership is the boundary: an agent running as you cannot give itself trust. Entries:
  ```json
  { "authorities": [
      { "name": "kisi-ci", "key": "ed25519:…", "native": false,
        "network": { "hosts": ["api.github.com", "*.githubusercontent.com"] },
        "fs": { "read": ["$CWD", "$HOME/data"], "write": ["$CWD"] },
        "expires": "2027-06-30" } ] }
  ```
  - `sudo zazz trust add NAME KEY [--native] [--hosts …] [--read …] [--write …] [--expires …]`, `sudo zazz trust rm NAME`, `zazz trust ls`.
  - Host patterns allow only a single leading `*.` label.
- **Global ceiling:** `/etc/zazzle/policy.json`, with the same ownership rules; a built-in default applies when it is missing. It caps filesystem access for every tier and can turn off native or network entirely (`"native": false`, `"network": false`).

## 6. `zazz add PATH`

1. Open with `O_RDONLY|O_NOFOLLOW|O_CLOEXEC`; the caller must already be able to read the file. Require a regular file within the size limit. Read it **once** into memory (D2).
2. `hash = BLAKE3(bytes)`; the id is `b3:<64 hex>`. Commands accept any unique prefix of at least 12 hex characters.
3. If the artifacts for `hash` exist and the build key matches, reply `cached`.
4. Otherwise take an exclusive `flock` on `locks/<hash>.lock` and check step 3 again.
5. Validate the archive and manifest. Verify `zazz.sig` if present and record the signer key; trust is not decided here (§3).
6. Extract from memory into a fresh `tmp/<hash>.<pid>/` (0700, every file `O_CREAT|O_EXCL|O_NOFOLLOW`).
7. Build according to `kind` and trust (§7), inside the compile sandbox.
8. Instantiate the result once in its runtime and fetch `openapi` if declared; cache it in the metadata.
9. Publish atomically (temp file + rename): the metadata and the artifacts.
10. Delete `tmp/<hash>.<pid>/`, which holds the sources and the original `.wasm` copy, then release the lock.

**What stays:** compiled artifacts only, never the `.zazz` itself:
- `wasm/<hash>.wasm`: the validated module, or the module we compiled from Rust source;
- `wasm/<hash>.cwasm`: the AOT cache, which can be rebuilt from `.wasm` after a wasmtime upgrade;
- `bin/<hash>`: the native binary;
- `meta/<hash>.json`.

A native binary cannot be rebuilt once the toolchain changes; it is marked `stale` and keeps running until it is re-added.

**Store** `~/.zazzle/v1/zazz/` (0700): `wasm/`, `bin/`, `meta/`, `names/`, `locks/`, `tmp/` (anything older than 1 h is purged at start-up) and `sdk/<buildkey>/`.

## 7. Building

### WASM (no toolchain needed)
- Validate the module with wasmtime and check its imports against an allowlist:
  - `wasi_snapshot_preview1` or WASI 0.2 `cli`, `filesystem`, `clocks`, `random` and `io`;
  - `wasi:http/outgoing-handler` only if the network tier can be granted.
  - Unknown imports fail with a list of the offending names.
- AOT-compile to `.cwasm` with Cranelift in `zazz-worker compile`, under the compile sandbox (untrusted WASM is parsed only inside a sandbox). Bytes come in and the `.cwasm` goes out over pipes.
- **Build key:** wasmtime version + target + CPU features + `zazz-worker` version.

### Rust → WASM (default for `kind: "rust"`)
- `rustc --edition E --target wasm32-wasip1 --crate-type bin -C opt-level=s --error-format=json --extern zazz=sdk/<key>/wasm32-wasip1/libzazz.rlib src/main.rs`, then the WASM steps above.
- This needs `rustup target add wasm32-wasip1`; `doctor` checks for it.
- **Toolchain lookup:** `$ZAZZ_RUSTC`, then `~/.cargo/bin/rustc`, then `/opt/homebrew/bin/rustc`. Never `PATH`, which is empty under launchd.

### Rust → native (signed, authority trusted for native, global allows native)
- Same `rustc` call for the host target with `-C opt-level=2`. The linker is resolved once to an absolute clang (via `xcrun -f clang`) and cached in the build key.

### All builds
- Argument lists only, a 120 s timeout and a cleared environment.
- Diagnostics are condensed to `{"level","file","line","col","msg","hint"}`, at most 20.
- **Compile sandbox:** read only the toolchain, the SDK, the build directory and the system SDK; write only the build directory; no network. This blocks `include_str!` exfiltration.
- No `build.rs`, Cargo dependencies or proc-macros for Rust source; crates come in through the `.wasm` route (D1).

## 8. Running

### Processes
- **Pool key** = (hash, runtime, effective policy instantiated for the caller's cwd). The OS sandbox is fixed when a process starts, so a worker never serves a different cwd or policy.
- **WASM:**
  - `zazzled` spawns `zazz-worker serve` under the OS sandbox and passes the `.cwasm` as a descriptor, so the worker needs no read access to the store.
  - The worker mmaps the module once and makes a **fresh wasmtime `Store` and instance per call**, using the pooling allocator, so calls share no state.
  - Limits: `StoreLimits` for memory and epoch interruption for `call_ms`.
- **Native:** `bin/<hash>` is spawned directly under the OS sandbox and serves calls in sequence through the SDK.
- **Both:**
  - the process gets the caller's cwd, fds 0/1/2 on `/dev/null`, **fd 3** as the control socketpair and a scrubbed environment (`LANG`, `TMPDIR`/`HOME` = per-worker tmp, `ZAZZ_PROTO=1`);
  - before exec: `setsid`, `RLIMIT_CORE=0`, `RLIMIT_NOFILE`, `RLIMIT_FSIZE`, and closing every descriptor above 3.
- **Recycling:** a worker is retired after 60 s idle or 1000 calls, when its RSS is over the limit, after a protocol error or timeout, or when there are more than 32 warm workers (least recently used first). Shutdown is `{"t":"shutdown"}`, then SIGKILL after 200 ms. Scripts must not rely on state between calls.
- **Daemon threads:** calls run on dedicated threads, never on the accept workers that serve `find`/`read`. When the caller hangs up (`POLLHUP`), zazzled sends `cancel` and kills the worker after 200 ms. When zazzled exits, it shuts down every worker.

### Protocol: one message set, three transports

Messages (JSON):

| message | reply |
|---|---|
| `{"t":"hello","proto":1}` | `{"t":"ready","messages":[…],"sdk":"…"}` |
| `{"t":"json-command","id":N,"mode":"agent"\|"human","input":{…},"deadline_ms":…}` | `{"t":"result","id":N,"ok":true,"output":…,"exit":0}` or `{"ok":false,"error":{"code","message"}}` |
| `{"t":"openapi","id":N}` | an OpenAPI 3.1 document in `output` |
| `{"t":"service-call","id":N,"operation":"…","params":{…},"body":…}` | as `json-command` |
| `{"t":"progress","id":N,"msg":"…"}` (script → launcher, optional) · `{"t":"cancel","id":N}` · `{"t":"shutdown"}` | |

Transports:

1. **zazzled ↔ native script, and zazzled ↔ `zazz-worker`:** frames of `u32` LE length + JSON on fd 3, with the caller's fds 0/1/2 attached to each call via `SCM_RIGHTS`. The descriptor-passing code in `server.rs` moves to `fdpass.rs`.
2. **`zazz-worker` ↔ WASI command module** (works for every language with WASI):
   - per call, a fresh tmp directory is preopened as `/.zazz/`, holding `request.json` (the message above);
   - the guest writes `/.zazz/response.json`;
   - the guest's stdin/stdout/stderr **are the caller's real fds**, wrapped as WASI streams, with terminal flags set from `isatty`;
   - `proc_exit` becomes `exit` when there is no response.
3. **`zazz-worker` ↔ component (later phase):** a WIT world `zazz:script@1` that exports `handle: func(request: string) -> string` and carries the same messages without temp files.

- **Agent mode:** zazzled prints `output` as JSON on the caller's stdout once the result arrives. Scripts use stderr for anything else.
- **MCP** has no fds: zazzled makes pipes, feeds the tool's `stdin` string and returns stdout and stderr capped to the reply budget.
- The **SDK** (`zazz` crate, std-only, two backends from one API: fd-3 frames for native, `/.zazz/` files for wasip1) handles:
  - framing, `catch_unwind`, a small `Json` type;
  - `Call::{stdin, stdout, stderr, read_input("-"), println, progress, is_human, cancelled}`.
- Error codes the launcher adds: `script_crashed`, `timeout`, `out_of_memory`, `sandbox_denied`, `network_denied` (with the host), `untrusted_signer`, `protocol`, `busy`.

## 9. Access control: two layers

### WASI layer (WASM tiers)
- **Filesystem:** preopens only. `$CWD` is read-only unless write is granted; there is also `/.zazz/` and the per-worker tmp. Effective set = global ∩ manifest (∩ authority when signed).
- **Network:**
  - **Unsigned:** no `wasi:http` or `wasi:sockets` is linked at all, so a module that imports them fails at `add`.
  - **Signed:**
    - `wasi:http/outgoing-handler` is linked with a `send_request` hook that allows only `https` to hosts in manifest ∩ authority (exact or `*.` match), with no redirects to other hosts and no IP literals;
    - TLS is done by the host;
    - raw `wasi:sockets` stays off in v1.
- No process spawning exists in WASI; clocks and random are allowed.

### OS layer (all tiers)
- **macOS:** SBPL generated from a fixed template, with every path passed as `-D` (`(subpath (param "CWD"))`), never by string interpolation. `/usr/bin/sandbox-exec` is called by absolute path. Rules:
  - deny by default; read the system libraries and the policy paths; write only `$TMP` and the granted paths;
  - no fork or exec;
  - `network-outbound` to `*:443` only for WASM workers of network-granted scripts (the WASM layer narrows it per host), or for native scripts granted network;
  - JIT: allow `dynamic-code-generation` only for `zazz-worker`.
- **Linux:** Landlock rulesets for the same paths; seccomp denies `socket`, `connect` and `fork` unless granted; `PR_SET_NO_NEW_PRIVS`; an unshared network namespace when network is denied.
  - WASM tiers are refused without Landlock and seccomp (D3).
  - Signed native runs with what is available and is flagged in `status`.

## 10. The `zazz` command

The **invoker** is the existing C binary, dispatched on `basename(argv[0])`:
- `zazzle` works as today;
- `zazz` prepends `zazz` to the args;
- any other name (a shim symlinked to `zazz`) prepends `zazz exec-name <argv0>`.

The release ships the `zazz` symlink and the `zazz-worker` binary. zazz requests are never session-deduped.

| agent command | reply |
|---|---|
| `zazz pack DIR [-o FILE] [--sign KEY]` | `{"path","hash","bytes","signed"}`; validates locally |
| `zazz add PATH` | `{"hash","status":"compiled"\|"cached","runtime":"wasm"\|"native","signer","network","warnings":[…]}` or `{"error":{"code":"compile_failed","diagnostics":[…]}}` |
| `zazz call HASH [JSON\|-]` / `zazz call HASH OPERATION [--params JSON] [--body JSON\|-]` | the output |
| `zazz describe HASH` | manifest summary, tier, effective policy, operations |
| `zazz ls` · `zazz rm HASH` · `zazz gc` · `zazz status` · `zazz doctor` · `zazz new NAME [--wasm]` | short JSON |

- **Trust commands:** `zazz keygen`, `zazz sign`, `zazz trust ls|add|rm`. `trust add` and `trust rm` need root and are never offered over MCP.
- **MCP:** `zazz_add`, `zazz_call`, `zazz_describe` and `zazz_ls`. Exposing each script's operations as MCP tools comes later.
- **Humans:**
  - `zazz install PATH|HASH [--as NAME]` creates the shim `~/.zazzle/bin/NAME -> zazz`; also `zazz uninstall NAME`, `zazz list` and `zazz run NAME ARGS…`.
  - **Argument parsing:** the launcher parses `ARGS` against `cli.args`. Types: `bool`, `count`, `string`, `int`, `float`, `enum` and `path` (made absolute against the cwd; `-` means stdin). Each can be `multiple`, have a `default` or be `required`.
  - `--` and combined short flags work, and `--help` is generated. Parse errors exit 2.
  - Without a `cli` section, flags are derived from the OpenAPI input schema; with neither, the script gets `{"argv":[…]}`.
  - **Output:** the script writes straight to the real stdout. If it returns an `output`, the launcher renders it: a string raw, flat object arrays as a table on a TTY or TSV otherwise, anything else as JSON. `--json` forces raw JSON.
  - Ctrl-C gives exit 130 after `cancel`.

## 11. Security checklist

- **Hostile archives:** limits are checked before allocating, names before extracting, and every file is opened `O_EXCL|O_NOFOLLOW`.
- **D2:** a single read; the hash, signature check and build all use the same bytes.
- **Untrusted code paths:** Cranelift and `rustc` only ever see untrusted input inside the compile sandbox. No build scripts or proc-macros.
- **Trust and policy files:** owned by root and checked on every load. The authority key, grants and expiry are checked at every launch.
- **Unsigned scripts:** no network capability linked at all; WASM and the OS sandbox both confine the filesystem.
- **Sandbox profiles:** SBPL from parameters only; scrubbed environment; no inherited descriptors above 3; per-call WASM instances.
- **Shims and output:** shim names cannot shadow subcommands. Script output and diagnostics are capped to a budget before they reach an agent.

## 12. Code layout

```
macos/rust/zazz-sdk/          std-only SDK crate (native + wasip1 backends), embedded in zazzled
macos/rust/zazz-worker/       wasmtime host: serve (calls) + compile (AOT); the only crate with wasmtime
macos/rust/zazzled/src/fdpass.rs
macos/rust/zazzled/src/zazz/
  mod.rs  archive.rs  manifest.rs  sign.rs  trust.rs  registry.rs  compile.rs
  policy.rs  sbpl.rs  landlock.rs  wire.rs  pool.rs  cliargs.rs  render.rs
macos/c/zazzle/src/zazzle_core.h   argv[0] dispatch
agents/skills/zazz/SKILL.md
```

- **New dependencies:**
  - in zazzled: `miniz_oxide` (zip) and `ed25519-dalek` (signatures);
  - in zazz-worker: `wasmtime`, `wasmtime-wasi` and `wasmtime-wasi-http`.
- Keeping wasmtime out of zazzled keeps the daemon's size, start time and coverage as they are.

## 13. Phases (each ships green: fmt, clippy, tests, coverage ≥ 95%)

1. **Archive, manifest, `pack`.** Tests for malicious zips and determinism.
2. **Signing and trust:** `sign.rs`, `trust.rs`, `keygen`/`sign`, policy intersection, as pure functions with full tests.
3. **SDK:** both backends, tested over a socketpair and with wasip1 under wasmtime.
4. **`zazz-worker`:** AOT compile, per-call instances, fd-backed stdio, `/.zazz/` transport, limits, the http hook with host allowlist.
5. **Registry and builds:** WASM validation and AOT, Rust → wasip1, Rust → native for trusted signers, locks, atomic publish, diagnostics.
6. **Pool and wire:** spawn, recycle, cancel, timeouts, fd forwarding, dedicated call threads.
7. **OS sandbox:** SBPL generator (pure) and macOS backend, the compile sandbox, Linux Landlock + seccomp. A test matrix per tier (fs outside scope, network, host not allowed, fork).
8. **Agent front ends:** argv[0] dispatch, the subcommands, MCP tools, `doctor`, `new`.
9. **Humans:** install, shims, `cliargs`, `--help`, renderer, Ctrl-C.
10. **`openapi`/`service-call`** caching and `describe`; CLI derived from the schema; then the component transport.
11. **Docs and release:** README, skill, AGENTS.md line, `zazz` symlink, `zazz-worker` in the tarball and formula, signing entitlements.

## 14. Risks and spikes (do first)

- **wasmtime under `sandbox-exec`:** loading `.cwasm` needs executable memory. Find the minimal SBPL (`dynamic-code-generation`?) and, for notarized releases, the hardened-runtime entitlement (`com.apple.security.cs.allow-jit` or `allow-unsigned-executable-memory`) for `zazz-worker` only.
- **SCM_RIGHTS and received-fd I/O** in a deny-default profile, and the minimal `mach-lookup` set.
- **Latency:** worker cold start (sandbox + exec + module mmap) and per-call instantiation with the pooling allocator. The aim is for a warm call to cost about a socket round trip plus tens of µs.
- **MSRV:** wasmtime follows recent Rust. Expect to raise `rust-version` for the workspace, or give `zazz-worker` its own (CI `msrv` job).
- **macOS linker:** native and wasip1 builds need the Command Line Tools or rustup targets; `.wasm` scripts need neither.
- **`sandbox-exec` deprecation:** keep the backend behind `policy.rs`; fallback is `sandbox_init_with_parameters`.
- **Coverage:** macOS-only and wasmtime paths are measured on the matching CI job; keep the decision logic in pure functions.

## 15. Open decisions

- **D6 Where trust lives.**
  - Recommended: root-owned `/etc/zazzle/trust.json` + `policy.json`, managed with `sudo zazz trust …`, because an agent running as you cannot then grant itself trust.
  - Alternative: `~/.zazzle/`, which is convenient but writable by any unsandboxed agent running as you.
- **D7 Signature format.**
  - Recommended: our own Ed25519 (`zazz keygen`/`sign`).
  - Alternatives: SSH signatures (`ssh-keygen -Y sign`, reuses existing git signing keys, `allowed_signers`-style trust), or Apple Developer ID / codesign identities.
- **D8 WASM runtime.**
  - Recommended: wasmtime in a separate `zazz-worker` binary (Cranelift AOT, WASI 0.2, wasi-http hook, epochs, pooling).
  - Alternative: `wasmi`, an interpreter: small and pure Rust, but much slower and without wasi-http.
