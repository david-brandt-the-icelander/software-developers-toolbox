# AGENTS.md

Personal developer toolbox: small macOS command-line tools written in Rust, plus the C# tool `zazzle-csharp-indexer`.

## Rules
- Rust: follow `macos/rust/style.md`. The Cargo workspace is `macos/rust/`.
- C#: follow `macos/dotnet/style.md`. The tool is `macos/dotnet/zazzle-csharp-indexer/` (commands in its README).
- Every code file, struct, enum and function has exactly one plain-sentence `//` comment; no rustdoc except clap `///` help text.
- Write code for low token use and easy agent comprehension.
- Never commit `target/` or state/log files.
- Editing AGENTS.md or skills: follow `agents/directives.md`.

## Commands (run in `macos/rust/`)
- Build: `cargo build --release`
- Before committing: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- Single test: `cargo test -p free-disk-space test_parse_size_suffixes`
- C invoker (`macos/c/zazzle/`): build the daemon first, then in that folder `make && make unit && sh test/smoke.sh`; `make coverage` gates C branch coverage; `sh bench/run.sh` measures latency.
- zazzled branch coverage: `sh zazzled/coverage.sh 95` in `macos/rust`.
- Branch coverage (nightly `llvm-tools-preview` + `cargo-llvm-cov`; target 95% of non-test branches and lines, enforced in CI for agent-fs and fix-network): `cargo +nightly llvm-cov --branch -p <crate> --summary-only`; run free-disk-space as non-root so its permission tests execute.
- CI (`.github/workflows/ci.yml`) runs the same on macOS and Linux; pushing a `v*` tag publishes universal macOS binaries to a GitHub release, icelander.net/downloads and the `zazzcat/homebrew-z` tap (`release.yml`; secrets `CLOUDFLARE_R2_TOKEN`, `HOMEBREW_TAP_TOKEN`, optional signing in `macos/release/README.md`).
- Release scripts: `bash macos/release/sign-and-notarize.test.sh && bash macos/release/formula.test.sh` (also `shellcheck` them).

## Skills
- Free disk space on macOS (compression scan, cache/dev/docker cleanup): `agents/skills/free-disk-space/SKILL.md`
- Diagnose/repair macOS Wi-Fi, DNS, VPN: `agents/skills/fix-network/SKILL.md`
- Read/search files with cached JSON replies instead of cat/grep (`afs`): `agents/skills/agent-fs/SKILL.md`
- Read, search, outline and safely edit files with `zazzle` instead of cat/grep/sed: `agents/skills/zazzle/SKILL.md`

## Layout
- `macos/rust/<tool>/` one crate per tool; `macos/c/zazzle/` is the freestanding C invoker for `zazzled`; `macos/dotnet/<tool>/` one .NET project per C# tool; `agents/` agent docs and skills.
