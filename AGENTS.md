# AGENTS.md

Personal developer toolbox: small macOS command-line tools written in Rust.

## Rules
- Rust: follow `macos/rust/style.md`. The Cargo workspace is `macos/rust/`.
- Every code file, struct, enum and function has exactly one plain-sentence `//` comment; no rustdoc except clap `///` help text.
- Write code for low token use and easy agent comprehension.
- Never commit `target/` or state/log files.
- Editing AGENTS.md or skills: follow `agents/directives.md`.

## Commands (run in `macos/rust/`)
- Build: `cargo build --release`
- Before committing: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- Single test: `cargo test -p free-disk-space test_parse_size_suffixes`
- Branch coverage (nightly + cargo-llvm-cov; run as non-root so permission tests execute; target 95% of non-test code): `cargo +nightly llvm-cov --branch -p free-disk-space`
- CI (`.github/workflows/ci.yml`) runs the same on macOS and Linux; pushing a `v*` tag publishes universal macOS binaries (`release.yml`).

## Skills
- Free disk space on macOS (compression scan, cache/dev/docker cleanup): `agents/skills/free-disk-space/SKILL.md`
- Diagnose/repair macOS Wi-Fi, DNS, VPN: `agents/skills/fix-network/SKILL.md`

## Layout
- `macos/rust/<tool>/` one crate per tool; `agents/` agent docs and skills.
