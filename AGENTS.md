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
- Branch coverage (nightly `llvm-tools-preview` + `cargo-llvm-cov`; target 95% of non-test branches and lines, enforced in CI for agent-fs and fix-network): `cargo +nightly llvm-cov --branch -p <crate> --summary-only`; run free-disk-space as non-root so its permission tests execute.
- CI (`.github/workflows/ci.yml`) runs the same on macOS and Linux; pushing a `v*` tag publishes universal macOS binaries to a GitHub release, icelander.net/apps and the Homebrew formula (`release.yml`; secret `CLOUDFLARE_R2_TOKEN`, optional signing in `macos/release/README.md`).
- Release scripts: `bash macos/release/sign-and-notarize.test.sh && bash macos/release/formula.test.sh` (also `shellcheck` them).

## Skills
- Free disk space on macOS (compression scan, cache/dev/docker cleanup): `agents/skills/free-disk-space/SKILL.md`
- Diagnose/repair macOS Wi-Fi, DNS, VPN: `agents/skills/fix-network/SKILL.md`
- Read/search files with cached JSON replies instead of cat/grep (`afs`): `agents/skills/agent-fs/SKILL.md`

## Layout
- `macos/rust/<tool>/` one crate per tool; `agents/` agent docs and skills.
