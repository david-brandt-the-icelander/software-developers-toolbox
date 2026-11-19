# AGENTS.md

zazzle: a launcher (`zazzle`, short name `z`) and the macOS tools it runs, in Rust, C and C#.

## Rules
- Rust: follow `src/macos/rust-style.md`. The Cargo workspace is `src/macos/`.
- C#: follow `src/macos/csharp-style.md`. The tools are `src/macos/csharp-indexer/` and `src/macos/csharp-minifier/` (commands in their READMEs).
- Every code file, struct, enum and function has exactly one plain-sentence `//` comment; no rustdoc except clap `///` help text.
- Write code for low token use and easy agent comprehension.
- Never commit `target/` or state/log files.
- Editing AGENTS.md or skills: follow `agents/directives.md`.
- Each tool is one folder `src/macos/<name>` with a plain descriptive name; only the launcher is called zazzle.
- Only `zazzle` and `z` go on PATH; every other binary installs to `libexec/zazzle/` and runs as `zazzle TOOL ARGS`. A new tool needs an entry in `TOOLS` (`src/macos/zazzle/src/zazzle_core.h`), `release/pack.sh` callers, `release/bottle.sh`, `release/formula.sh` and `release.yml`.
- Name a new Rust crate and its binary after its folder (`zazz-script/runner` is crate `zazz-script-runner`).
- Every tool answers `--version` (`NAME VERSION` on the first line, which `zazzle tools` reads) and `help`; a tool that keeps state has `status [--json]` whose JSON starts with `tool`, `version` and `ok`.

## Commands (run in `src/macos/`)
- Build: `cargo build --release`
- Before committing: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- Single test: `cargo test -p free-disk-space test_parse_size_suffixes`
- git-tools (`zazzle git`, its commit flow and message editor): `cargo test -p git-tools` includes `tests/pty.rs`, which runs the programs on a pseudo-terminal; CI gates it at 95% lines and branches.
- C launcher (`src/macos/zazzle/`): build the daemon first (`cargo build --release -p daemon`), then in that folder `make && make unit && sh test/smoke.sh`; `make coverage` gates C branch coverage; `sh bench/run.sh` measures latency.
- daemon branch coverage: `sh daemon/coverage.sh 95` in `src/macos` (also `95 zazz-script-packaging`, `95 zazz-script-engine`, `95 zazz-script-runner`).
- Local models (`zazzle agent llm|prompt`, `src/macos/ai-agent`, llama.cpp): not in the tarball; the daemon downloads it on first use, checked against `ZAZZLE_TOOLS_MANIFEST` (`release/tools.sh`). Pin a new model in `src/catalog.rs` with the commit, size and SHA-256 from Hugging Face's API, never by hand; `cargo test -p ai-agent` needs no model.
- Language modules: grammars not built into the daemon are `src/macos/language-modules` cdylibs (one per feature); the daemon's tests link them statically and build the Lua module once; release builds embed their hashes via `ZAZZLE_LANGUAGES_MANIFEST` (`release/language-modules.sh`).
- zazz tests need `rustup target add wasm32-wasip1` (stable and nightly) and build `zazz-script-runner` (release) on first use; `zazz-script-runner` needs Rust 1.97, the rest of the workspace 1.85.
- Branch coverage (nightly `llvm-tools-preview` + `cargo-llvm-cov`; target 95% of non-test branches and lines, enforced in CI for shell-tools, fix-network, git-tools and ai-agent-rules): `cargo +nightly llvm-cov --branch -p <crate> --summary-only`; run free-disk-space as non-root so its permission tests execute.
- CI (`.github/workflows/ci.yml`) runs the same on macOS and Linux; running the Release workflow (bump patch, minor or major from the latest `v*` tag, or an exact `version`; `channel: alpha` for only the alpha) or pushing a `vX.Y.Z` tag first publishes an alpha (`vX.Y.Z-alpha.N`, the `zazzle-alpha` formula) from a quick build, then publishes universal macOS binaries to a GitHub release without notes, icelander.net/downloads and the `zazzcat/homebrew-z` tap; the tag is the only version source, the repository says `0.0.0-dev` (`release.yml`; secrets `CLOUDFLARE_R2_TOKEN`, `HOMEBREW_TAP_TOKEN`, optional signing in `release/README.md`).
- Release scripts: `bash release/sign-and-notarize.test.sh && bash release/formula.test.sh && bash release/version.test.sh && bash release/fetch-cyberspace-generator.test.sh && bash release/language-modules.test.sh && bash release/tools.test.sh && bash release/r2-put.test.sh` (also `shellcheck` them; `formula.test.sh` covers `pack.sh` and `bottle.sh`).
- `cyberspace-generator` is built in the cyberspace-explorer repository, not here: change it there. `bump-cyberspace-generator.yml` opens the PR that moves `release/cyberspace-generator.version`; never edit that pin by hand to an unreleased tag.
- `src/macos/lempel-ziv-finite-state-entropy-compression/` is a vendored copy of the lzfse-compression library that free-disk-space compiles in (`free-disk-space/build.rs`): change it in that repository, then copy its `include/` and `src/` here unchanged and update the commit in that folder's README.

## Skills
- Free disk space on macOS (compression scan, cache/dev/docker cleanup): `agents/skills/free-disk-space/SKILL.md`
- Diagnose/repair macOS Wi-Fi, DNS, VPN: `agents/skills/fix-network/SKILL.md`
- Read, search, outline and safely edit files with `zazzle` instead of cat/grep/sed; commit with `zazzle git commit -m`: `agents/skills/zazzle/SKILL.md`
- Write, test and run sandboxed zazz scripts (`zazzle zazz new|test|pack|add|call`): `agents/skills/zazz/SKILL.md`
- Keep one test file per method under test in repositories using `csharp-minifier --split-tests`: `agents/skills/csharp-minifier/SKILL.md`
- Get the rules and skills that apply to a path with `zazzle agent get-rules PATH`, or ask a local open model with `zazzle agent prompt` (or `zazzle agent chat`): `agents/skills/zazzle-agent/SKILL.md`
- Validate and maintain a repository's `.dev` files with `cyberspace-generator`: `agents/skills/cyberspace-generator/SKILL.md`

## Architecture diagrams and knowledge files
- Each maintained folder documents itself in a `.dev/` directory as JSON following the skills in `Meta/Agents/Skills/` and the catalogue in `Meta/Agents/Knowledge/` (human-assisted software development). Read a folder's `.dev` files before changing it.
- `populate-dev` decides what a folder needs. Each tool is its own unit; there is no diagram across tools, and grouping folders (`src`, `src/macos`, `src/macos/zazz-script`, `agents`) get only a `folder` record.
- `describe-component-diagram`: each tool folder (`src/macos/<tool>`), its `src`, and source folders with parts of their own (daemon's `src/cmd`, `src/proxy`, `src/wrap`; the indexer's `src/Analysis`, `src/Daemon`, `src/Index`, `src/Tools`).
- `describe-class-diagram`: a folder that itself declares more than one struct, enum, trait or class (each `src`, daemon's `src/cmd`, `src/proxy`, `src/wrap`, the indexer's `src/*` except `Mcp`).
- `describe-state-diagram`: a folder that implements a state machine; one `<Identifier>.state-diagram.json` per machine, named after its type (free-disk-space `Cand` and `ScanState`, git-tools `Editor`, the daemon's chat `Editor`).
- `describe-deployment-diagram`: each tool folder that builds a binary someone installs (`src/macos/<tool>`).
- Releases are `.github/.dev` (pipelines, the release use case) and `release/.dev` (delivery to the GitHub release, icelander.net/downloads and the `zazzcat/homebrew-z` tap).
- What a folder does, offers and exposes: `describe-use-case`, `describe-user-journey`, `describe-capability`, `describe-feature`, `describe-operation`, linked with `relatedViews`.
- Intent, promises and needs: knowledge files via `describe-knowledge`. Wanted work is a `backlog` item in the folder that will own it; a question only the owner can answer is a `decisions` record with `status: open`.
- Every actor a view names has a file in `Actors/`, written with `describe-actor` and linked from what names it; delete an actor nothing links to.
- Update `.dev` files and actors in the same commit as the code; delete one that stops applying.
- The skills and the catalogue come from <https://github.com/david-brandt-the-icelander/human-assisted-software-development> (`Agents/Skills`, `Agents/Knowledge`): change them there and copy them here unchanged.
- Before finishing, with `jsonschema` installed, from the repository root: `python3 Meta/Agents/Skills/validate_diagrams.py` and `python3 Meta/Agents/Knowledge/validate_knowledge.py`.

## Layout
- `src/macos/zazzle/` the C launcher; `src/macos/daemon/` the daemon it talks to; `src/macos/<tool>/` every other tool (Rust crates, the .NET projects `csharp-indexer` and `csharp-minifier`).
- `src/macos/zazz-script/{packaging,engine,runner}/` the zazz script format, SDK and WASM host the daemon uses; `src/macos/language-modules/` the grammars the daemon downloads; `src/macos/ai-agent/` local open models, a tool the daemon downloads.
- `src/macos/shell-tools/` is a library the daemon links for `zazzle call` and `zazzle stat` (JSON ops, file-version memory); it has no program of its own.
- `src/macos/git-tools/` builds `git-tools`, `git-tools-commit` and `git-tools-commit-message-editor` (`zazzle git`); `src/macos/ai-agent-rules/` is the rule lister behind `zazzle agent get-rules`.
- `release/` release scripts; `agents/` agent docs and skills; `Actors/` the actors the `.dev` views name; `Meta/Agents/` the HASD copy.
