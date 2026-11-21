# Release: Homebrew formula and optional signing

`release.sh` in [continuous_integration_and_delivery](https://github.com/david-brandt-the-icelander/continuous_integration_and_delivery/tree/main/zazzle) runs these scripts: on the owner's Apple Silicon Mac it builds, tests, signs and packs the universal macOS binaries and the C# indexer, in a Linux Docker container it builds the Linux tarballs, and once everything has built it publishes them to a GitHub release (with no release notes), icelander.net/downloads and the Homebrew tap. Its README has every option and the credentials it reads. CI is `ci.sh` in the same folder, run by hand in that container; its `lint` stage shellchecks these scripts and runs their tests.

## Version numbers (`version.sh`, `set-version.sh`)

- The git tag is the only source of version numbers; the repository's crates and the indexer say `0.0.0-dev`.
- Run `release.sh --bump patch|minor|major`: `version.sh` takes the latest `vX.Y.Z` tag (v0.0.0 when there is none) and computes the next one, and publishing creates that tag on the released commit with the GitHub release. Give `--version X.Y.Z` (a tag that doesn't exist yet) to release exactly that version instead of a bump. `--dry-run` builds and tests everything and publishes nothing. Pushing a tag yourself releases nothing.
- `set-version.sh X.Y.Z` writes the version into every crate's manifest and its `Cargo.lock` entry (no network, so `--locked` still holds); the indexer gets `-p:Version=X.Y.Z`. Every binary's `--version` then reports the release.
- `version.test.sh` checks the arithmetic (v1.10.0 sorts after v1.2.9) and the refusals; CI's `lint` stage runs it with shellcheck.

## Alpha channel

- Every release run first publishes an alpha of the same commit, `vX.Y.Z-alpha.N` (N one past the version's alpha tags), from a quick build on the Mac: no tests and no PGO profile, otherwise the same macOS binaries, language modules, indexer and signing. There is no Linux alpha.
- It goes to a GitHub pre-release, to icelander.net under `zazzle/vX.Y.Z-alpha.N/` and `zazzle/alpha/`, and to the tap as `Formula/zazzle-alpha.rb` (class `ZazzleAlpha`, which `conflicts_with "zazzle"`), with its own bottle. Install it with `brew install zazzcat/z/zazzle-alpha`; `brew upgrade` follows later alphas.
- The release builds start once the alpha is published, so it is there to try while they run; a failed alpha stops the release.
- `--channel alpha` publishes only the alpha. With `--version`, it may name a version that is already released (`--version 1.2.28 --channel alpha` gives `v1.2.28-alpha.N`).

## Homebrew formula (`formula.sh`, `bottle.sh`)

`release.sh` writes `Formula/zazzle.rb` into a checkout of the tap repository `zazzcat/homebrew-z` and pushes it to that repo's default branch, after the GitHub release and the uploads.

- One formula installs the launcher `zazzle`, the only file it links, and every tool unlinked in its `libexec/zazzle`: `daemon`, `zazz-script-runner`, `fix-network`, `free-disk-space`, `git-tools`, `git-tools-commit`, `git-tools-commit-message-editor`, `ai-agent-rules` and `cyberspace-generator` (universal), and `csharp-indexer`. People run each as `zazzle TOOL`. The C# indexer is not universal, so the tarball holds it gzipped for both CPU types (`libexec/indexer/csharp-indexer-arm64.gz`, `-x64.gz`), and the formula's `post_install_steps` write `libexec/zazzle/csharp-indexer` for this Mac (`brew postinstall zazzle` reruns that).
- `post_install_steps` also link `HOMEBREW_PREFIX/bin/z` to `opt/zazzle/bin/zazzle`, removed again on uninstall. They first remove a `z` of zazzle's own (either channel), then make none when another `bin/z`, `/usr/local/bin/z`, Homebrew's `zoxide` or `etc/profile.d/z.sh` (the `z` formula) exists, or when `HOMEBREW_PREFIX/etc/zazzle/no-z` asks for none. Creating that file and running `brew postinstall zazzle` takes an existing `z` away.
- `pack.sh DIST OUT FILE...` packs every tarball (macOS and Linux) in that layout: `bin/zazzle`, `bin/z` (a relative link), the tools in `libexec/zazzle/` and the gzipped indexers in `libexec/indexer/`.
- `bottle.sh` packs the bottle (`zazzle-<version>.all.bottle.tar.gz`, one, because the binaries are universal) from the unpacked tarball: the same keg as the formula's `install`, without `z`, which post-install decides. `release.sh` runs it in its Linux container (it needs GNU tar) and uploads it to the GitHub release. The formula's `bottle` block points there, so `brew install` pours it without Homebrew's build-from-source checks, which demand up-to-date Command Line Tools. The release tarball stays the formula's source fallback.
- The formula takes the release tag as its version, so every release is an upgrade.
- Homebrew doesn't quarantine what a formula installs, so unsigned binaries open without a Gatekeeper prompt.
- Users install with:
  ```bash
  brew install zazzcat/z/zazzle
  ```
  brew taps `https://github.com/zazzcat/homebrew-z` by itself because the repo is named `homebrew-<tap>`.
- One-time setup: `release.sh` pushes to the tap with your own git credentials, or with `HOMEBREW_TAP_TOKEN` when it is set: a fine-grained personal access token on the `zazzcat` account with "Contents: read and write" on `zazzcat/homebrew-z` only, kept in the environment or in `~/.config/zazzle-release/env`. The tap needs at least one commit on its default branch.
- `formula.test.sh` checks the generated Ruby against a stand-in Homebrew `Formula` class, for both CPU types:
  - every DSL value, the installed files, the indexer step run against a fake keg, and the `z` link steps run against a fake prefix (made when free, kept as zazzle's own, never over another `z`, removed on opt-out);
  - `pack.sh`'s layout;
  - the bottle block: `root_url`, the `all` tag, `:any_skip_relocation`, and a checksum matching the bottle;
  - that the bottle exists under the name Homebrew downloads (`Bottle::Filename#url_encode`);
  - that the bottle holds exactly `zazzle/VERSION/...` (Homebrew's `Bottle#extract_snapshot` check) with only `zazzle` in `bin`, the binaries mode 0755, and is reproducible.

  CI's `lint` stage runs it. The stand-in is not Homebrew: nothing has pulled a real bottle until a release runs.

## Profile-guided optimization of daemon's C code (`pgo.sh`)

Tree-sitter's C code dominates parsing, so the release compiles it with clang PGO. Rust code is not profiled: Rust's own PGO needs an `llvm-profdata` that matches rustc's LLVM, which Xcode's tools don't guarantee.

- `pgo.sh [CORPUS]` builds an instrumented `daemon` (`-fprofile-instr-generate`, for the host target only, so build scripts stay uninstrumented) under `src/macos/target/pgo`.
- It trains that build by running `outline` and `refs` over the crate sources that building `daemon` downloaded (`$CARGO_HOME/registry/src`). Then it merges the profile with the matching `llvm-profdata` (Xcode's on macOS) and prints `PGO_CFLAGS=...`.
- The release builds with `CC=clang` and `CFLAGS=$PGO_CFLAGS`. On macOS the arm64-trained profile also serves the x86_64 slice, because tree-sitter's C code is the same on both.
- Linux builds need `clang`, `llvm` and `libclang-rt-<version>-dev` (the profile runtime); the toolchain image in continuous_integration_and_delivery has them.
- `release.sh` builds the macOS release on the Mac in one sequence: the language modules and JIT tools (signed, then their manifests), the workspace tests, the profile, both CPU types with it, the launcher, the indexer, then signing and the tarball. It then builds each Linux tarball in its container, and publishes only once all of them have built.
- CI's optional `bench` stage (`ci.sh bench`) times `refs` with and without the profile, on Linux only. It trains and measures on the same crate sources, so its PGO numbers are an upper bound.

## Signing and notarization (`sign-and-notarize.sh`, optional)

Only browser downloads need signing, since Homebrew installs don't. With the Apple credentials below, the release signs every binary with a Developer ID (hardened runtime, secure timestamp) and has Apple notarize them before packing. Without them, it releases unsigned binaries with a warning.

- To make a missing credential fail the release instead, set `REQUIRE_SIGNING=true`.
- `sign-and-notarize.test.sh` checks the script against stand-in Apple tools; CI's `lint` stage runs it with shellcheck. Nothing has signed a real binary until a release runs with the credentials in place.

### One-time signing setup

You need a paid [Apple Developer Program](https://developer.apple.com/programs/) membership. The Account Holder does steps 1 and 3.

1. **Developer ID Application certificate**, on a Mac:
   - In Keychain Access, choose Certificate Assistant → Request a Certificate From a Certificate Authority. Save the request to disk.
   - On developer.apple.com, open Certificates → + → **Developer ID Application** → G2 Sub-CA, and upload the request.
   - Download the `.cer` file and double-click it to add it to your login keychain.
2. **Export it as .p12.** In Keychain Access → My Certificates, right-click "Developer ID Application: …" → Export, pick `.p12` and set a password. Then:
   ```bash
   base64 -i DeveloperID.p12 | pbcopy
   ```
3. **App Store Connect API key.** In App Store Connect, open Users and Access → Integrations → App Store Connect API → Team Keys → +, with the role **Developer**.
   - Download `AuthKey_<KEYID>.p8`; Apple offers it only once.
   - Note the **Key ID** and the **Issuer ID** shown above the list.
4. **Add them to the release environment**: `~/.config/zazzle-release/env` on the Mac that runs `release.sh` (keep it out of git, `chmod 600`), which `release.sh` reads as bash, or the environment `release.sh` runs in:
   ```bash
   MACOS_CERTIFICATE_P12=...         # the base64 from step 2
   MACOS_CERTIFICATE_PASSWORD=...    # the .p12 password
   APPLE_API_KEY_P8="..."            # the contents of AuthKey_KEYID.p8, quoted because it spans lines
   APPLE_API_KEY_ID=...              # the Key ID
   APPLE_API_ISSUER_ID=...           # the Issuer ID
   ```
5. Delete the local `.p12` and the base64 copy. Keep the `.p8` somewhere safe, or revoke it and make a new one if it leaks.

### Checking a signed release

```bash
codesign -dv --verbose=2 fix-network 2>&1 | grep -E "Authority=Developer ID Application|flags=.*runtime|Timestamp"
```

Then download the tarball in a browser, unpack it, and run `./fix-network --help`. It should start without a warning.

Notarization can't be stapled to a bare executable. The first launch on each Mac checks the notarization with Apple online.

## Language modules (`language-modules.sh`)

All 34 tree-sitter grammars ship as language modules (the daemon links none) (`src/macos/language-modules`, one cdylib per Cargo feature), which the daemon downloads on demand and checks against a manifest compiled into it.

- `language-modules.sh OUT [TARGET...]` builds every module into `OUT/NAME.dylib` (macOS, universal with `lipo` when given both Apple targets) or `OUT/NAME.so` (Linux, the host). `LANGUAGES="lua go"` limits it.
- `language-modules.sh --manifest OUT` writes `OUT/manifest`, one `NAME SHA256 FILE` line per module.
- The release builds the modules, signs them on macOS with `sign-and-notarize.sh` (a signed, hardened the daemon only loads libraries signed by the same team), writes the manifest, then builds daemon with `ZAZZLE_LANGUAGES_MANIFEST` pointing at it, so the hashes are those of the signed files. It uploads the modules and the manifest to `icelander-downloads/zazzle/vX.Y.Z/languages/{macos-universal,linux-x86_64,linux-aarch64}/`, under the tag only: a daemon accepts only its own release's modules.
- `pgo.sh` needs the modules: the daemon has no grammars, so the release builds them first and points `PGO_LANGUAGES` at them; training loads them while it profiles the tree-sitter runtime in daemon (the modules themselves are not profiled).
- `language-modules.test.sh` builds the Lua module and checks its manifest line; CI's `lint` stage runs it with shellcheck.

## JIT tools (`tools.sh`)

Programs zazzle runs that are too large or too rarely wanted for the tarball are JIT tools: the daemon downloads them on first use and checks them against a manifest compiled into it, like the language modules. The only one is `ai-agent` (`src/macos/ai-agent`, llama.cpp), behind `zazzle agent llm` and `zazzle agent prompt`; it is not in the tarball, the bottle or the formula.

- `tools.sh OUT [TARGET...]` builds every tool into `OUT/NAME`: universal with `lipo` when given both Apple targets (with Metal for Apple Silicon), else for the host. `TOOLS="ai-agent"` limits it; `PROFILE=dev` builds unoptimized.
- `tools.sh --manifest OUT` writes `OUT/manifest`, one `NAME SHA256 FILE` line per tool.
- The release builds the tools right after the language modules, signs them on macOS, writes the manifest, then builds daemon with `ZAZZLE_TOOLS_MANIFEST` pointing at it (workspace builds pass `--exclude ai-agent`, so llama.cpp is built once). It uploads them with the manifest to `icelander-downloads/zazzle/vX.Y.Z/tools/{macos-universal,linux-x86_64,linux-aarch64}/`, under the tag only.
- `tools.test.sh` builds ai-agent (dev profile) and checks its manifest line; CI's `lint` stage runs it with shellcheck.

## Uploads to icelander.net (`r2-put.sh`)

`r2-put.sh KEY FILE CONTENT_TYPE` uploads one file to the `icelander-downloads` R2 bucket with `wrangler`. Cloudflare's API sometimes times out (HTTP 524), so it tries up to five times, waiting 15, 30, 60 and 120 seconds in between. Publishing uploads to the GitHub release when it already exists instead of creating it, and every upload overwrites. `version.sh` refuses `--version X.Y.Z` once its tag exists (except with `--channel alpha`), so a release that failed after its GitHub release was created cannot simply be run again with that version. `r2-put.test.sh` checks the retries against a stand-in `npx`.

## cyberspace-generator (`fetch-cyberspace-generator.sh`)

`cyberspace-generator` is built and released in [cyberspace-explorer](https://github.com/david-brandt-the-icelander/cyberspace-explorer) (tags `generator-v*`). `cyberspace-generator.version` pins the release zazzle ships. `release.sh` runs `fetch-cyberspace-generator.sh`, which downloads that release's binaries and checks them against its `SHA256SUMS`; the macOS build merges the two CPU types with `lipo` and signs the result with the other binaries, and each Linux tarball gets the static musl build.

`bump-cyberspace-generator.sh` in continuous_integration_and_delivery, run by hand, looks for a newer `generator-v*` release. When there is one, it pins it on a branch, checks that the release has every binary zazzle needs (`fetch-cyberspace-generator.sh`), runs CI on the branch, and only when CI passes pushes the branch and opens the pull request. Merge it, then release zazzle to ship it. `fetch-cyberspace-generator.test.sh` checks the script against a fake release.
