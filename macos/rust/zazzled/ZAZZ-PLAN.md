# Plan: `zazz` scripts for zazzle(d)

Status: draft, not implemented. Revision 3:
- scripts run as WASM by default, and as native Rust only when signed by an accepted authority;
- trust lives in the OS store (Keychain) and is checked off the hot path;
- `.zazz` files carry unit tests that must pass once per machine;
- a standard test library fakes the edges (filesystem, network, stdio, clock);
- `keygen` and `sign` are scripts themselves.

All decisions are settled (§2).

## 1. What it is

Agents write a small program, pack it into one compressed file (`NAME.zazz`) and give zazzled its absolute path. zazzled then:
- hashes it;
- builds it;
- **runs its unit tests once on this machine**;
- from then on runs it by hash through a fixed JSON protocol.

How a script runs:
- **Default:** as **WebAssembly** (wasmtime) inside a worker process that is itself OS-sandboxed (`sandbox-exec` on macOS, Landlock + seccomp on Linux). That gives two layers of access control: WASI/host imports, then the OS sandbox.
- **Signed by an accepted authority:** the script may run as **native Rust**, limited only by the OS sandbox.

Humans can install the same scripts as ordinary commands with parsed flags and normal stdin/stdout.

```
agent:  zazz new csv-sum && $EDITOR csv-sum/src/main.rs csv-sum/tests/*.rs
        zazz test ./csv-sum                               # build + run tests, nothing registered
        zazz pack ./csv-sum -o /tmp/csv-sum.zazz          # deterministic, our own deflate
        zazz add /tmp/csv-sum.zazz                        # build, test once, register -> {"hash":"b3:9f2c…",…}
        zazz call 9f2c41d0a7e1 '{"files":["a.csv"]}'      # json in, json out
signer: zazz sign /tmp/gh-sync.zazz                       # key in the Keychain / Secure Enclave
human:  zazz install /tmp/csv-sum.zazz && csv-sum -d ';' a.csv | sort
```

## 2. Settled decisions

- **D1 WASM in place of Rust source:** a `.zazz` may carry a prebuilt `.wasm` built from any language and any WASM-compatible crates. It may also carry Rust source, which we compile ourselves (std + SDK only).
- **D2 What we read is what we build:** the file is read once into memory. The hash, signature check, extraction, build and tests all use those same bytes. The path is never opened again.
- **D3 Linux:** unsigned scripts need Landlock + seccomp and are refused without them. Signed native scripts run with whatever sandboxing the OS offers.
- **D4 Network:**
  - A script gets network access only when its manifest asks for it **and** it is signed by an accepted authority.
  - An authority's host and filesystem limits are enforced by the WASM layer; they do not apply to native Rust.
- **D5 Script id:** BLAKE3 of the exact `.zazz` bytes.
- **D6 Trust store:** wherever the OS keeps trust.
  - **macOS:** the Keychain, which covers MDM- and AD-managed certificates as well as user trust settings.
  - **Linux:** root-owned `/etc/zazzle/trust.d/`.
- **D7 Signature format:** our own envelope. `keygen` and `sign` run through zazzled as zazz scripts (§6).
- **D8 WASM runtime:** wasmtime, in a separate `zazz-worker` binary.
- **Trust is never on the critical path:**
  - A trust verdict is cached for `trust_ttl` seconds and refreshed in the background.
  - Launches use the last verdict and never wait.
  - Revocation is done by the external system (for example AD/MDM removing or distrusting a certificate). We pick it up at the next refresh.
- **Tests:** a `.zazz` should include unit tests. A script whose tests fail is never run. Tests run once per script hash per machine.
- **Test library:** included automatically. It makes edge interactions (filesystem, network, stdio, clock, cancellation) easy to fake and assert.
- **D9 Signature algorithm:** ECDSA P-256. It works with the Secure Enclave, Keychain/SecTrust, AD CS and MDM-issued certificates.
- **D10 After a refresh says "revoked":** native scripts fall back to their WASM build, and WASM scripts drop to the unsigned tier (no network). A launch is never blocked. The default `trust_ttl` is 600 s.
- **D11 Tests are optional:** `add` warns on `tests: 0`. An administrator can still require tests with the global policy switch `require_tests` (off by default). This applies to native scripts too.
- **D12 Built-ins are prebuilt:** `keygen`, `sign` and `trust` ship as prebuilt universal native binaries signed by the pinned release authority. `native-bin` is accepted only from that authority, so the built-ins work on machines with no Rust toolchain.
- **Earlier fixed points still hold:**
  - one compressed file, decompressed with our own built-in code;
  - no copy of the `.zazz` is kept;
  - script processes start on demand and are recycled at our discretion;
  - the protocol is `json-command` (required) plus `openapi`/`service-call` (recommended);
  - the shell command is `zazz`; the file extension is `.zazz`.

## 3. Execution tiers

| | unsigned / not accepted | accepted, WASM | accepted for `native`, Rust |
|---|---|---|---|
| runtime | WASM in `zazz-worker` | WASM in `zazz-worker` | native binary (a WASM build is kept as fallback) |
| access-control layers | host imports/WASI + OS | host imports/WASI + OS | OS only |
| network | never | manifest hosts ∩ authority hosts, checked per request by the host | OS-level on/off; host list not enforced |
| filesystem | global ∩ manifest | global ∩ manifest ∩ authority | global ∩ manifest |
| Linux without Landlock | refuse | refuse | run, flagged in `status` |

### Trust verdicts (off the hot path)

- **What a verdict holds:** `{signer, authority, grants, checked_at, valid_until, source}`.
  - It is first computed during `add`, which is not latency-sensitive, and persisted in `meta/<hash>.json`.
  - It is kept in memory by zazzled.
- **Launch:** reads the verdict from memory (or from the metadata after a daemon restart) and **never blocks on it**. An expired verdict is still used for that launch, and a refresh is queued.
- **Refresher:** a background thread in zazzled re-evaluates verdicts of recently used scripts every `trust_ttl` (default 600 s, set in the global policy). On macOS that is `SecTrustEvaluateWithError`, which may do OCSP/CRL network I/O; that is why it stays off the hot path.
- **When a refresh changes a verdict:**
  - the metadata is updated;
  - new launches use the new pool key (§8);
  - workers under the old verdict drain and are recycled.
- **Revoked or expired signer** (D10):
  - native scripts fall back to their WASM build;
  - WASM scripts drop to the unsigned tier (no network).
  - Nothing that was runnable becomes unrunnable because of a trust lookup.

## 4. The `.zazz` file

- **Format:** ZIP, limited to stored and deflate entries. Inflate and deflate use `miniz_oxide`, built in; CRC-32 is our own table. `zazz pack` is deterministic: sorted entries, fixed timestamps, level 9.
- **Limits:**
  - at most 32 MiB compressed, 128 MiB uncompressed, 1024 entries, 64 MiB per entry, and a compression ratio of at most 100:1;
  - no zip64, encryption, multi-disk archives or symlinks;
  - names must be relative UTF-8 with `/` only; no `..`, empty or `.` components, backslashes or NUL, and no two names equal after case folding.
- **Layout:**
  ```
  zazz.json              manifest (required)
  zazz.sig               signature envelope (optional, §5)
  src/**.rs              kind "rust": sources
  tests/**.rs            kind "rust": unit/integration tests, compiled with --test against the test library
  NAME.wasm              kind "wasm": the module
  NAME.tests.wasm        kind "wasm": the test module (any language, §9)
  tests/fixtures/**      read-only files for tests (preopened as the test cwd)
  tests/http.json        recorded request -> response pairs for the fake network
  tests/snapshots/**     golden outputs for assert_snapshot
  ```

### Manifest `zazz.json`

```json
{
  "zazz": 1,
  "name": "gh-sync",
  "version": "0.1.0",
  "description": "Sync issue labels from a JSON file.",
  "kind": "rust",
  "entry": "src/main.rs",
  "native": false,
  "tests": { "paths": ["tests"], "timeout_ms": 120000 },
  "messages": ["json-command", "openapi", "service-call"],
  "sandbox": { "read": ["$CWD"], "write": [], "network": { "hosts": ["api.github.com"] }, "exec": false },
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

- For `kind: "wasm"`, `tests` is `{"module": "NAME.tests.wasm"}`.
- `network` is `"none"` (the default), `{"hosts": [...]}` or `"all"`.

## 5. Signing and trust

- **Envelope** `zazz.sig` (our own format):
  ```json
  {"v":1,"alg":"ecdsa-p256-sha256","digest":"b3:<hex>","sig":"<base64 DER>","chain":["<base64 DER cert>", "…"]}
  ```
  - `digest` is the BLAKE3 hash of every other entry, encoded canonically: sorted by name, then for each entry the name, the length as `u64` LE and the bytes.
  - The signature covers `zazz-sig-v1\0` followed by the digest.
  - `chain` is the signer's certificate and any intermediate certificates, so trust can be evaluated without a network lookup on the hot path.
- **Algorithm** (D9): ECDSA P-256. It is the only curve the Secure Enclave supports, and the one AD Certificate Services, MDM profiles and `SecTrust` handle everywhere. Verification uses the `p256` crate on both operating systems.
- **Authority = a certificate trusted for zazz signing:**
  - The leaf carries our Extended Key Usage OID `zazz-code-signing`.
  - An optional custom extension holds the grants as JSON: `{"native":bool,"network":{"hosts":[…]}|"all"|"none","fs":{"read":[…],"write":[…]}}`. Without it, the default grant is no native and no network.
- **macOS trust:** `SecTrustCreateWithCertificates` with the chain, plus the basic X.509 policy and our EKU check. Anchors and trust settings come from the Keychain:
  - an org CA pushed by MDM or AD;
  - a user's self-signed authority whose trust was set by `zazz trust add`. macOS itself asks the human to authenticate before it changes trust settings, so an agent cannot quietly grant itself trust.
  - Revocation (OCSP/CRL, MDM removing a certificate, AD) is whatever the OS does; we only re-evaluate on schedule.
- **Linux trust:** PEM anchors in root-owned `/etc/zazzle/trust.d/*.pem`, chain validation with `rustls-webpki` (P-256, custom EKU), and CRLs in `/etc/zazzle/crl.d/` when present.
- **Global ceiling:** a root-owned policy at the OS's natural location: `/Library/Managed Preferences/net.icelander.zazz.plist`, deployable by MDM, or `/etc/zazzle/policy.json` on Linux. It holds the filesystem ceiling, `trust_ttl`, `require_tests`, and switches to disable native or network. A built-in default applies when it is missing.

## 6. Built-in scripts: `keygen`, `sign`, `trust`

- `zazz keygen`, `zazz sign` and `zazz trust` are **zazz scripts that run through zazzled** and its normal registry, pool and protocol.
- They are embedded in the zazzled binary as `.zazz` bytes and registered on first use.
- They are signed by the **zazzle release authority**, whose public key is pinned in zazzled. That authority's grant is fixed in code and covers only what these scripts need:
  - Keychain access (`mach-lookup` for `securityd`/`ctkd`);
  - reading the input path;
  - writing the output path.
- **`keygen [--name N] [--csr]`:** creates a non-exportable P-256 key in the Secure Enclave (or as a Keychain software key where no enclave exists), then either:
  - a self-signed authority certificate with the zazz EKU, or
  - a CSR for the org CA.
  - On Linux the key is a 0600 file in `~/.zazzle/keys/`.
- **`sign FILE [-o OUT] [--key N]`:** reads the archive, computes the digest, signs with the Keychain key and writes a new `.zazz` that includes `zazz.sig`. The input file keeps its own hash; the output gets a new one.
- **`trust add CERT [--grants …]` / `trust ls` / `trust rm`:** set Keychain trust settings for our policy (macOS prompts the human); on Linux this needs root.
- **Exception to D2's build-from-source rule** (D12): these built-ins carry a **prebuilt universal native binary**, because a machine may have no Rust toolchain. Only the pinned release authority may ship `kind: "native-bin"`. Its signature covers the binary, so it is still exactly what we read.

## 7. `zazz add PATH`

1. Open with `O_RDONLY|O_NOFOLLOW|O_CLOEXEC`; the caller must be able to read the file. Require a regular file within the limit. Read it **once** (D2).
2. `hash = BLAKE3(bytes)`; the id is `b3:<64 hex>`; commands accept unique prefixes of at least 12 hex characters.
3. If the artifacts and a **passing test record** exist for `hash` with a matching build key, reply `cached`.
4. Otherwise take an exclusive `flock` on `locks/<hash>.lock` and check step 3 again.
5. Validate the archive and manifest. If `zazz.sig` is present, verify the signature over the in-memory bytes and compute the first trust verdict.
6. Extract from memory into a fresh `tmp/<hash>.<pid>/` (0700, every file `O_EXCL|O_NOFOLLOW`).
7. Build (§8), inside the compile sandbox: the script, and its tests.
8. **Run the tests** (§9) in the script's runtime tier, but always with no network and a scratch copy of `tests/fixtures/` as cwd.
   - Any failure: nothing is published, and the reply lists the failures.
   - Pass: record `{passed, failed:0, duration_ms, build_key}`.
   - No tests: `"tests":0` with a warning, or refusal when the global policy sets `require_tests` (D11).
9. Fetch `openapi` if declared and cache it.
10. Publish atomically (temp file + rename): the metadata, the verdict, the test record and the artifacts.
11. Delete the temp directory (sources, test binaries, fixtures) and release the lock.

**What stays:**
- `wasm/<hash>.wasm` and `wasm/<hash>.cwasm` (the AOT cache, which can be rebuilt from `.wasm`);
- `bin/<hash>` (native, plus its WASM fallback build);
- `meta/<hash>.json`.

A `.cwasm` rebuilt after a wasmtime upgrade does not re-run tests (same module); a new native build is impossible without the sources, so the native binary is marked `stale`. The store lives in `~/.zazzle/v1/zazz/` (0700).

## 8. Building

- **WASM:**
  - Validate the module. Imports must be from the allowlist: `wasi_snapshot_preview1`, WASI 0.2 `cli`/`filesystem`/`clocks`/`random`/`io`, our `zazz_host` module (§10), and `wasi:http/outgoing-handler` only for signed scripts.
  - AOT-compile to `.cwasm` in `zazz-worker compile`, under the compile sandbox.
  - **Build key:** wasmtime version + target + CPU features + worker version.
- **Rust → WASM** (default):
  - `rustc --edition E --target wasm32-wasip1 --crate-type bin -C opt-level=s --error-format=json --extern zazz=<sdk rlib> src/main.rs`.
  - Tests: `rustc --test …` per test file, the same way, linked with the SDK's `zazz::test` module.
  - Requires `rustup target add wasm32-wasip1`; `doctor` checks for it.
- **Rust → native** (accepted for native, and policy allows native): the same commands for the host target, with the linker resolved once to an absolute clang. **The WASM build is produced as well**, so a revoked trust verdict can fall back to it (§3). Tests run for both builds.
- **Toolchain lookup:** `$ZAZZ_RUSTC`, then `~/.cargo/bin/rustc`, then `/opt/homebrew/bin/rustc`; never `PATH`, which is empty under launchd.
- **All builds:** argument lists only, a 120 s timeout and a cleared environment. Diagnostics are condensed to `{level,file,line,col,msg,hint}`, at most 20.
- **Compile sandbox:** read the toolchain, the SDK and the build directory; write only the build directory; no network. No `build.rs`, Cargo dependencies or proc-macros; crates come in through `.wasm` (D1).

## 9. Tests and the test library

### Running tests
- **When:** once per script hash per machine, during `add`, and also with `zazz test DIR|FILE` (build + test, nothing registered) for the agent's edit loop.
- **Where:** the same runtime tier the script will use. Network is always off; the only network is the fake one. The cwd is a scratch copy of `tests/fixtures/`. Time limit: `tests.timeout_ms`.
- **Rust:** the libtest harness built with `--test`. We parse `test NAME ... ok|FAILED|ignored` lines and the failure sections into `{"passed":N,"failed":[{"name","message","file","line"}],"ignored":N}`, with at most 20 failures and every message capped.
- **Other languages:** `NAME.tests.wasm` is a WASI command. It reports through `zazz_host.send` as `{"t":"test-report","passed":N,"failed":[…]}`. Without a report, exit status 0 counts as passed.
- **Never run:** a hash without a passing record cannot be called (`tests_failed` / `tests_missing`).

### `zazz::test`, the edge-faking library (part of the SDK, always available to tests)

**Production side:** the SDK routes edge I/O through the call context, so tests can swap implementations:
- `call.fs()` (open, read, write, list, metadata);
- `call.http()` (request → response);
- `call.stdin()/stdout()/stderr()`, `call.clock()`, `call.env()`, `call.cancelled()`.

Plain `std::fs` still works in production but cannot be faked.

**Test side:**
- **`Harness`** builds a fake call context and runs the script's handlers exactly as the launcher would:
  - `h.json_command(run, json!({…}))` returns `Result<Json>`;
  - `h.service_call("op", params, body)`;
  - `h.human(&["-d", ";", "a.csv"])` uses the **same `cliargs` parser as the launcher** and the manifest's `cli` spec, then checks stdout, stderr and the exit code.
- **`FakeFs`:**
  - an in-memory tree built from literals, or `from_fixtures("tests/fixtures/x")`;
  - records every write (`h.fs().written("out.csv")`);
  - can inject errors: `deny("secrets/")`, `fail_write("out", Io::NoSpace)`.
- **`FakeHttp`:**
  - expectations: `expect(GET, "https://api.github.com/repos/a/b").respond(200, json!(…))`, or replay from `tests/http.json`;
  - an unmatched request fails the test with the full request shown;
  - `h.http().requests()` lets the test assert on what was sent;
  - helpers for latency and timeouts.
- **Policy simulation:** `Harness::from_manifest()` applies the manifest's `sandbox` to the fakes. A read outside `read` returns `sandbox_denied`, and a request to a host not in `network.hosts` returns `network_denied`, with the same error codes as production. Policy mistakes show up in tests, not in use.
- **Other fakes:** `FakeStdio` (stdin bytes, captured output, `tty(true)`), `FakeClock` (set or advance), `FakeEnv`, and `h.cancel_after(n)` to test cancellation.
- **Assertions:**
  - `assert_json_eq!` (reports the paths that differ);
  - `assert_snapshot!("name", value)` against `tests/snapshots/name.json` (`zazz test --update` rewrites them locally; `add` never updates them);
  - `assert_schema(output, "operationId")` checks the output against the script's OpenAPI document (a JSON Schema subset: type, properties, required, items, enum).
- **Any language through the host:** for WASM test modules, `zazz-worker` in test mode serves `zazz_host.http` from `tests/http.json` and preopens the scratch fixtures as cwd, so the network and filesystem fakes work without our SDK.

## 10. Running

### Processes
- **Pool key** = (hash, runtime, verdict id, effective policy instantiated for the caller's cwd).
- **WASM:**
  - zazzled spawns `zazz-worker serve` under the OS sandbox and passes the `.cwasm` as a descriptor;
  - a **fresh `Store` and instance per call**, with the pooling allocator;
  - limits: `StoreLimits` for memory, epoch interruption for `call_ms`.
- **Native:** `bin/<hash>` under the OS sandbox serves calls in sequence through the SDK.
- **Both:**
  - the caller's cwd, fds 0/1/2 on `/dev/null`, **fd 3** as the control socketpair, and a scrubbed environment;
  - `setsid`, rlimits, and every descriptor above 3 closed before exec.
- **Recycling:** a worker is retired after 60 s idle or 1000 calls, when RSS is over the limit, after an error or timeout, when its verdict changes, or when there are more than 32 warm workers (LRU).
- **Daemon threads:** calls run on dedicated threads, never on the accept workers. On `POLLHUP` from the caller: `cancel`, then SIGKILL after 200 ms.

### Protocol: one message set
- `hello`/`ready`
- `json-command` (required)
- `openapi`
- `service-call`
- `progress`
- `cancel`, `shutdown`
- `test-report`
- results `{"t":"result","id":N,"ok":…,"output"|"error":…,"exit":…}`

### Transports
1. **zazzled ↔ native script and zazzled ↔ `zazz-worker`:** frames of `u32` LE length + JSON on fd 3, with the caller's fds 0/1/2 attached to each call via `SCM_RIGHTS` (code moves from `server.rs` to `fdpass.rs`).
2. **`zazz-worker` ↔ WASM guest:** **host imports** in the core-wasm module `zazz_host`, usable from any language without bindings:
   - `recv(ptr, cap) -> i64`: the call message, or `-needed`;
   - `send(ptr, len)`: result, progress or test-report;
   - `http(req_ptr, req_len, resp_ptr, resp_cap) -> i64`: JSON head + body. The allowlist is enforced here. Unsigned scripts get `network_denied`; the import is always linked, so behaviour is the same in tests and in production;
   - `cancelled() -> i32`.
   - The guest's stdin/stdout/stderr are the caller's real fds, wrapped as WASI streams with `isatty`-based terminal flags.
3. **Components (later):** the WIT world `zazz:script@1` mirrors these functions; `wasi:http` is linked only for signed scripts, with the same host check.

### Front ends
- **Agent mode:** zazzled prints `output` as JSON on the caller's stdout.
- **MCP:** pipes in place of fds, with output capped to the budget.
- **Launcher error codes:** `script_crashed`, `timeout`, `out_of_memory`, `sandbox_denied`, `network_denied`, `tests_failed`, `tests_missing`, `protocol`, `busy`.

## 11. Access control: two layers

- **WASM layer:**
  - filesystem only through preopens (global ∩ manifest ∩ authority);
  - network only through `zazz_host.http` / `wasi:http`, with `https` only, exact or `*.` host match, no redirects to other hosts and no IP literals;
  - no process spawning.
- **OS layer, macOS:**
  - SBPL from a fixed template; every path passed as `-D`;
  - deny by default; no fork or exec;
  - `network-outbound` to `*:443` only for network-granted WASM workers and native scripts;
  - `dynamic-code-generation` only for `zazz-worker`;
  - Keychain `mach-lookup` only for the built-in authority's scripts.
- **OS layer, Linux:** Landlock, seccomp, `NO_NEW_PRIVS`, and a network namespace when network is denied.

## 12. The `zazz` command

The **invoker** is the existing C binary, dispatched on `basename(argv[0])`:
- `zazzle` works as today;
- `zazz` prepends `zazz` to the args;
- any other name (a shim) prepends `zazz exec-name <argv0>`.

zazz requests are never session-deduped.

| command | reply |
|---|---|
| `zazz new NAME [--wasm]` | template: manifest, `src/main.rs`, `tests/basic.rs` using `zazz::test` |
| `zazz test DIR\|FILE [--update]` | `{"passed","failed":[…],"ignored","build":"ok"\|diagnostics}` |
| `zazz pack DIR [-o FILE]` | `{"path","hash","bytes"}` |
| `zazz add PATH` | `{"hash","status","runtime","tests":{…},"trust":{…},"warnings":[…]}` or an error with diagnostics or test failures |
| `zazz call HASH [JSON\|-]` · `zazz call HASH OP [--params] [--body]` | output |
| `zazz describe` · `ls` · `rm` · `gc` · `status` · `doctor` | short JSON |
| `zazz keygen` · `sign` · `trust ls\|add\|rm` | built-in scripts (§6) |

- **MCP:** `zazz_test`, `zazz_add`, `zazz_call`, `zazz_describe` and `zazz_ls`. `trust add`/`rm` are never offered over MCP.
- **Humans:**
  - `zazz install PATH|HASH [--as NAME]` creates the shim `~/.zazzle/bin/NAME`; also `uninstall`, `list`, `run`.
  - **Arguments:** parsed by the launcher against `cli.args` (shared with the test library), with `--help` generated.
  - **Output:** the script writes to the real stdout, or returned output is rendered (string raw, table on a TTY or TSV otherwise, JSON); `--json` forces raw JSON.
  - Ctrl-C gives exit 130.

## 13. Code layout

```
macos/rust/zazz-sdk/            std-only SDK + zazz::test (native and wasip1 backends), embedded in zazzled
macos/rust/zazz-worker/         wasmtime host: serve, compile, test mode (fake http, fixtures)
macos/rust/zazz-builtins/       keygen, sign, trust (built as prebuilt universal binaries, packed as signed .zazz)
macos/rust/zazzled/src/fdpass.rs
macos/rust/zazzled/src/zazz/
  mod.rs  archive.rs  manifest.rs  sign.rs  verdict.rs  trust_macos.rs  trust_linux.rs
  registry.rs  compile.rs  testrun.rs  policy.rs  sbpl.rs  landlock.rs  wire.rs  pool.rs  cliargs.rs  render.rs
macos/c/zazzle/src/zazzle_core.h   argv[0] dispatch
agents/skills/zazz/SKILL.md
```

- **New dependencies:**
  - zazzled: `miniz_oxide` (zip), `p256` (signature verification), `rustls-webpki` (Linux chain validation), `security-framework` (macOS SecTrust and Keychain; or our own small FFI behind `cfg(macos)`);
  - zazz-worker: `wasmtime`, `wasmtime-wasi`, `wasmtime-wasi-http`.

## 14. Phases (each ships green: fmt, clippy, tests, coverage ≥ 95%)

1. **Archive, manifest, `pack`.** Tests for malicious zips and determinism.
2. **SDK and `zazz::test`:** call context with edge traits, fakes, harness, assertions, `cliargs`; both backends.
3. **`zazz-worker`:** AOT compile, per-call instances, `zazz_host` imports, fd-backed stdio, limits, test mode.
4. **Build and test pipeline:** WASM validation, Rust → wasip1 (plus tests), the libtest report parser, `zazz test`, test records.
5. **Registry and `add`:** locks, atomic publish, `cached`, diagnostics.
6. **Signing and verdicts:** envelope, `p256`, verdict cache and refresher, macOS SecTrust, Linux webpki, the tier decision, native build + WASM fallback.
7. **Built-ins:** `zazz-builtins` (keygen in the Secure Enclave, sign, trust), the release authority, `native-bin`, release packaging.
8. **Pool and wire:** spawn, recycle on verdict change, cancel, timeouts, fd forwarding, dedicated threads.
9. **OS sandbox:** SBPL generator, macOS backend, compile sandbox, Linux Landlock + seccomp; a test matrix per tier.
10. **Front ends:** argv[0] dispatch, subcommands, MCP, `doctor`, `new`.
11. **Humans:** install, shims, rendering, `--help`, Ctrl-C.
12. **`openapi`/`service-call`**, `assert_schema`, CLI derived from the schema; then components.
13. **Docs and release:** README, skill, AGENTS.md line, `zazz` symlink, `zazz-worker`, entitlements.

## 15. Risks and spikes (do first)

- **wasmtime under `sandbox-exec`:** executable memory for `.cwasm` (`dynamic-code-generation`) and the hardened-runtime entitlement for `zazz-worker` only.
- **Secure Enclave and Keychain from a sandboxed process:** the minimal `mach-lookup` set, and whether keychain access groups need a signed, entitled binary (likely: the built-ins must be codesigned with a keychain access group, which ties them to the release signing in `macos/release/`).
- **`SecTrust` with a custom EKU OID and a custom grants extension:** check that evaluation accepts them, and measure how long OCSP takes (it runs off the hot path, but it bounds refresh time).
- **SCM_RIGHTS and received-fd I/O** in a deny-default profile.
- **libtest on wasm32-wasip1:** check that panics, output capture and the report format parse reliably.
- **Latency:** worker cold start, per-call instantiation, and how long `add` takes now that it includes tests.
- **MSRV:** wasmtime will force a higher `rust-version`, at least for `zazz-worker`.
- **`sandbox-exec` deprecation:** keep the backend behind `policy.rs`.

## 16. Open decisions

None. D9–D12 are settled in §2. Implementation starts with the §15 spikes, then phase 1.
