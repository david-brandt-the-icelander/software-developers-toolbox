---
name: zazz
description: Write, test and run zazz scripts, small sandboxed programs (Rust compiled to WASM, or any WASM) that zazzled builds, tests once and runs by hash. Use when a task needs a reusable tool, a JSON-in/JSON-out helper, or a command for humans; or when the user mentions zazz, .zazz files, `zazz add`, `zazz call` or script signing.
---

# zazz scripts

Docs: `macos/rust/zazzled/README.md` (section "zazz scripts"); design: `macos/rust/zazzled/ZAZZ-PLAN.md`.

## Write
- `zazz new NAME` creates `NAME/zazz.json`, `NAME/src/main.rs` and `NAME/tests/basic.rs`; edit those.
- Rust scripts use only std and the `zazz` SDK: `zazz::serve(Handlers::new().json_command(run))`, where `run(call, input) -> Result<Json>`.
- Put the handlers in a `pub fn handlers()` so tests can call them (`use crate::script::handlers;`).
- Read and write through `call.fs()`, `call.http()`, `call.read_input(path)` ("-" is stdin) and `call.println`, not `std::fs`, so tests can fake them.
- Other languages or crates: build a WASI preview 1 command yourself and ship it with `"kind": "wasm"`; it gets the call message through the `zazz_host` imports `request_len`, `take` and `send`.

## Test
- Tests use `zazz::test::Harness::from_manifest(handlers(), include_str!("../zazz.json"))`.
- The harness fakes the edges: `h.fs().add(..)`, `h.http().expect_json(..)`, `h.stdin(..)`, `h.human(&[..])`. It applies the manifest's sandbox and fails on requests that weren't expected.
- `zazz test NAME` builds and runs the tests without registering anything; `--update` rewrites `tests/snapshots`.
- Fix compile errors from the `diagnostics` list (file, line, col, msg, hint) and failed tests from `tests.failed`.

## Ship and run
- `zazz pack NAME`, then `zazz add NAME.zazz`. The reply's `hash` (b3:...) is the script id; a second add of the same bytes says `cached`.
- Call with `zazz call ID '{"key": "value"}'` (or `-` to read JSON from stdin); `zazz call ID OP --params '{..}'` runs one OpenAPI operation.
- MCP clients use `zazz_new`, `zazz_test`, `zazz_pack`, `zazz_add`, `zazz_call`, `zazz_describe` and `zazz_ls`.
- `zazz describe ID` shows the sandbox, the operations and the signer; `zazz ls` lists scripts; `zazz rm ID` removes one.
- For humans: `zazz install NAME.zazz`, then `NAME ARGS` parses flags from the manifest's `cli` section.

## Sandbox
- A script reads only the manifest's `sandbox.read` paths (`$CWD` by default) and writes only `sandbox.write`. Unsigned scripts get no network.
- Network needs `"network": {"hosts": [..]}` in the manifest and a signature from an authority the machine trusts: `zazz keygen`, `zazz sign`, `sudo zazz trust add CERT`.
- `zazz doctor` reports what is missing (rustc, `rustup target add wasm32-wasip1`, zazz-worker, sandbox).
