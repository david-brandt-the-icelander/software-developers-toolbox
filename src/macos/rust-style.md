# Rust rules for agents

Sources: Rust API Guidelines, Clippy defaults, rustfmt. Condensed; repo rules win on conflict.

## Toolchain
- Stable Rust; MSRV is `rust-version` in `Cargo.toml` (CI checks it).
- `cargo fmt` and `cargo clippy -- -D warnings` must be clean; commit `Cargo.lock`.
- Prefer std; add a crate only with a stated reason. Shared settings live in `[workspace.package]`.

## Comments (repo rule)
- Every file, struct, enum, impl'd function and test gets exactly one `//` comment: one plain-English sentence saying what it is for. Files: first line. Items: the line above (above attributes).
- No rustdoc (`///`, `//!`) except clap help text on CLI args.
- Other comments only for a non-obvious why; never restate the code.

## Token-lean, agent-readable code
- Small single-purpose functions; flat control flow with early `return`/`continue`/`let else`.
- No dead code, no `#[allow]` to hide warnings, no speculative options.
- Order a file: `use`, constants, types, helpers, commands, `main`, then `#[cfg(test)] mod tests`.
- Keep files under ~800 lines; split into modules by responsibility.
- Print concise output; paths and numbers, not prose.

## Errors and safety
- No `unwrap()` outside tests and provably-valid literals (use `expect("why")`); return `Result` or report and exit.
- Every `unsafe` block gets a `// SAFETY:` comment; keep FFI behind small wrappers with `cfg(target_os = "macos")` fallbacks.
- Subprocesses: argument lists, never a shell string, always a timeout.
- Never follow symlinks implicitly when deleting; use `symlink_metadata`.

## Testing
- Unit tests in the same file, named `test_<unit>_<case>`; keep logic in pure functions so it is testable.
- Tests must pass on Linux too; gate macOS-only behaviour, not the tests.
