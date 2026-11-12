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
- Coverage: `cargo +nightly llvm-cov -p fix-network --branch --summary-only` (needs `cargo-llvm-cov` and nightly `llvm-tools-preview`); CI fails fix-network below 95% of branches or lines.
- CI (`.github/workflows/ci.yml`) runs the same on macOS and Linux; pushing a `v*` tag publishes universal macOS binaries to a GitHub release and to icelander.net/apps (`release.yml`; needs secret `CLOUDFLARE_R2_TOKEN`).

## Skills
- Free disk space on macOS (compression scan, cache/dev/docker cleanup): `agents/skills/free-disk-space/SKILL.md`
- Diagnose/repair macOS Wi-Fi, DNS, VPN: `agents/skills/fix-network/SKILL.md`

## Layout
- `macos/rust/<tool>/` one crate per tool; `agents/` agent docs and skills.
