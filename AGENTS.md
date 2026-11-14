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
- zazzled branch coverage: `sh zazzled/coverage.sh 95` in `macos/rust` (also `95 zazz-format`, `95 zazz-sdk`, `95 zazz-worker`).
- zazz tests need `rustup target add wasm32-wasip1` (stable and nightly) and build `zazz-worker` (release) on first use; `zazz-worker` needs Rust 1.97, the rest of the workspace 1.85.
- Branch coverage (nightly `llvm-tools-preview` + `cargo-llvm-cov`; target 95% of non-test branches and lines, enforced in CI for agent-fs and fix-network): `cargo +nightly llvm-cov --branch -p <crate> --summary-only`; run free-disk-space as non-root so its permission tests execute.
- CI (`.github/workflows/ci.yml`) runs the same on macOS and Linux; running the Release workflow (bump patch, minor or major from the latest `v*` tag) or pushing a `vX.Y.Z` tag publishes universal macOS binaries to a GitHub release without notes, icelander.net/downloads and the `zazzcat/homebrew-z` tap; the tag is the only version source, the repository says `0.0.0-dev` (`release.yml`; secrets `CLOUDFLARE_R2_TOKEN`, `HOMEBREW_TAP_TOKEN`, optional signing in `macos/release/README.md`).
- Release scripts: `bash macos/release/sign-and-notarize.test.sh && bash macos/release/formula.test.sh && bash macos/release/version.test.sh && bash macos/release/fetch-cyberspace-generator.test.sh` (also `shellcheck` them).
- `cyberspace-generator` is built in the cyberspace-explorer repository, not here: change it there. `bump-cyberspace-generator.yml` opens the PR that moves `macos/release/cyberspace-generator.version`; never edit that pin by hand to an unreleased tag.

## Skills
- Free disk space on macOS (compression scan, cache/dev/docker cleanup): `agents/skills/free-disk-space/SKILL.md`
- Diagnose/repair macOS Wi-Fi, DNS, VPN: `agents/skills/fix-network/SKILL.md`
- Read/search files with cached JSON replies instead of cat/grep (`afs`): `agents/skills/agent-fs/SKILL.md`
- Read, search, outline and safely edit files with `zazzle` instead of cat/grep/sed: `agents/skills/zazzle/SKILL.md`
- Write, test and run sandboxed zazz scripts (`zazz new|test|pack|add|call`): `agents/skills/zazz/SKILL.md`
- Validate and maintain a repository's `.dev` files with `cyberspace-generator`: `agents/skills/cyberspace-generator/SKILL.md`

## Architecture diagrams and knowledge files
- Each maintained folder documents itself in a `.dev/` directory as JSON following the skills in `Meta/Agents/Skills/` and the catalogue in `Meta/Agents/Knowledge/` (human-assisted software development). Read a folder's `.dev` files before changing it.
- `populate-dev` decides what a folder needs. Each tool is its own unit; there is no diagram across tools, and grouping folders (`macos`, `macos/rust`, `macos/c`, `macos/dotnet`, `agents`) get only a `folder` record.
- `describe-component-diagram`: each tool folder (`macos/rust/<tool>`, `macos/c/zazzle`, `macos/dotnet/zazzle-csharp-indexer`), its `src`, and source folders with parts of their own (zazzled's `src/cmd`, `src/proxy`, `src/wrap`; the indexer's `src/Analysis`, `src/Daemon`, `src/Index`, `src/Tools`).
- `describe-class-diagram`: a folder that itself declares more than one struct, enum, trait or class (each `src`, zazzled's `src/cmd`, `src/proxy`, `src/wrap`, the indexer's `src/*` except `Mcp`).
- `describe-state-diagram`: a folder that implements a state machine; one `<Identifier>.state-diagram.json` per machine, named after its type (agent-fs `TriIndex`, free-disk-space `Cand` and `ScanState`).
- `describe-deployment-diagram`: each tool folder that builds a binary someone installs (`macos/rust/<tool>`, `macos/c/zazzle`, `macos/dotnet/zazzle-csharp-indexer`).
- Releases are `.github/.dev` (pipelines, the release use case) and `macos/release/.dev` (delivery to the GitHub release, icelander.net/downloads and the `zazzcat/homebrew-z` tap).
- What a folder does, offers and exposes: `describe-use-case`, `describe-user-journey`, `describe-capability`, `describe-feature`, `describe-operation`, linked with `relatedViews`.
- Intent, promises and needs: knowledge files via `describe-knowledge`. Wanted work is a `backlog` item in the folder that will own it; a question only the owner can answer is a `decisions` record with `status: open`.
- Every actor a view names has a file in `Actors/`, written with `describe-actor` and linked from what names it; delete an actor nothing links to.
- Update `.dev` files and actors in the same commit as the code; delete one that stops applying.
- The skills and the catalogue come from <https://github.com/david-brandt-the-icelander/human-assisted-software-development> (`Agents/Skills`, `Agents/Knowledge`): change them there and copy them here unchanged.
- Before finishing, with `jsonschema` installed, from the repository root: `python3 Meta/Agents/Skills/validate_diagrams.py` and `python3 Meta/Agents/Knowledge/validate_knowledge.py`.

## Layout
- `macos/rust/<tool>/` one crate per tool; `macos/rust/zazz-{format,sdk,worker}/` are the zazz script format, SDK and WASM host used by zazzled; `macos/c/zazzle/` is the freestanding C invoker for `zazzled` (and `zazz`); `macos/dotnet/<tool>/` one .NET project per C# tool; `agents/` agent docs and skills; `Actors/` the actors the `.dev` views name; `Meta/Agents/` the HASD copy.
