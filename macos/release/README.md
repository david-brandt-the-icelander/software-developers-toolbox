# Release: Homebrew formula and optional signing

`.github/workflows/release.yml` builds universal binaries and the C# indexer and publishes them to a GitHub release (with no release notes), icelander.net/downloads and the Homebrew tap.

## Version numbers (`version.sh`, `set-version.sh`)

- The git tag is the only source of version numbers; the repository's crates and the indexer say `0.0.0-dev`.
- Run the Release workflow by hand with `bump` patch (default), minor or major: `version.sh` takes the latest `vX.Y.Z` tag (v0.0.0 when there is none), computes the next one, and the run tags the commit and publishes it. `bump: none` only builds. Pushing a `vX.Y.Z` tag yourself still releases that version.
- `set-version.sh X.Y.Z` writes the version into every crate's manifest and its `Cargo.lock` entry (no network, so `--locked` still holds); the indexer gets `-p:Version=X.Y.Z`. Every binary's `--version` then reports the release.
- `version.test.sh` checks the arithmetic (v1.10.0 sorts after v1.2.9) and the refusals; CI runs it with shellcheck.

## Homebrew formula (`formula.sh`, `bottle.sh`)

The `homebrew` job writes `Formula/zazzle.rb` into the tap repository `zazzcat/homebrew-z` and pushes it to that repo's default branch.

- One formula installs `zazzle`, `zazzled`, `afs`, `fix-network`, `free-disk-space` and `cyberspace-generator` (universal) and `zazzle-csharp-indexer`. The C# indexer is not universal, so the tarball holds it gzipped for both CPU types (`zazzle-csharp-indexer-arm64.gz`, `-x64.gz`). `bin/zazzle-csharp-indexer` starts as a placeholder so brew links it, and the formula's `post_install_steps` replace it with the binary for this Mac (`brew postinstall zazzle` reruns that).
- `bottle.sh` packs the bottle (`zazzle-<version>.all.bottle.tar.gz`, one, because the binaries are universal) and the job uploads it to the GitHub release. The formula's `bottle` block points there, so `brew install` pours it without Homebrew's build-from-source checks, which demand up-to-date Command Line Tools. The release tarball stays the formula's source fallback.
- The formula takes the release tag as its version, so every release is an upgrade.
- Homebrew doesn't quarantine what a formula installs, so unsigned binaries open without a Gatekeeper prompt.
- Users install with:
  ```bash
  brew install zazzcat/z/zazzle
  ```
  brew taps `https://github.com/zazzcat/homebrew-z` by itself because the repo is named `homebrew-<tap>`.
- One-time setup: create a fine-grained personal access token on the `zazzcat` account with "Contents: read and write" on `zazzcat/homebrew-z` only, and store it in this repository as the Actions secret `HOMEBREW_TAP_TOKEN`. The tap needs at least one commit on its default branch. `GITHUB_TOKEN` cannot push to another account's repository.
- `formula.test.sh` checks the generated Ruby against a stand-in Homebrew `Formula` class, for both CPU types:
  - every DSL value, the installed binaries, the `post_install` command and the `test` block;
  - that `release.yml` builds each binary;
  - the bottle block: `root_url`, the `all` tag, `:any_skip_relocation`, and a checksum matching the bottle;
  - that the bottle exists under the name Homebrew downloads (`Bottle::Filename#url_encode`);
  - that the bottle holds exactly `zazzle/VERSION/...` (Homebrew's `Bottle#extract_snapshot` check) with the binaries mode 0755, and is reproducible.

  CI runs it. The stand-in is not Homebrew: nothing has pulled a real bottle until a tag runs.

## Profile-guided optimization of zazzled's C code (`pgo.sh`)

Tree-sitter's C code dominates parsing, so the release compiles it with clang PGO. Rust code is not profiled: Rust's own PGO needs an `llvm-profdata` that matches rustc's LLVM, which Xcode's tools don't guarantee.

- `pgo.sh [CORPUS]` builds an instrumented `zazzled` (`-fprofile-instr-generate`, for the host target only, so build scripts stay uninstrumented) under `macos/rust/target/pgo`.
- It trains that build by running `outline` and `refs` over the crate sources that building `zazzled` downloaded (`$CARGO_HOME/registry/src`). Then it merges the profile with the matching `llvm-profdata` (Xcode's on macOS) and prints `PGO_CFLAGS=...` for `$GITHUB_ENV`.
- The release builds with `CC=clang` and `CFLAGS=$PGO_CFLAGS`. On macOS the arm64-trained profile also serves the x86_64 slice, because tree-sitter's C code is the same on both.
- Linux runners need `clang`, `llvm` and `libclang-rt-<version>-dev` (the profile runtime); the release job installs them.
- `.github/workflows/zazzle-bench.yml` times `refs` with and without the profile on macOS and Linux. It trains and measures on the same crate sources, so its PGO numbers are an upper bound.

## Signing and notarization (`sign-and-notarize.sh`, optional)

Only browser downloads need signing, since Homebrew installs don't. With the Apple secrets below, the release signs every binary with a Developer ID (hardened runtime, secure timestamp) and has Apple notarize them before packing. Without them, it releases unsigned binaries with a warning.

- To make a missing secret fail the release instead, set the repository variable `REQUIRE_SIGNING` to `true`.
- `sign-and-notarize.test.sh` checks the script against stand-in Apple tools; CI runs it with shellcheck. Nothing has signed a real binary until a tag runs with the secrets in place.

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
4. **Add the repository secrets** (Settings → Secrets and variables → Actions), or from a terminal:
   ```bash
   gh secret set MACOS_CERTIFICATE_P12        # paste the base64 from step 2
   ```
   ```bash
   gh secret set MACOS_CERTIFICATE_PASSWORD   # the .p12 password
   ```
   ```bash
   gh secret set APPLE_API_KEY_P8 < AuthKey_KEYID.p8
   ```
   ```bash
   gh secret set APPLE_API_KEY_ID             # the Key ID
   ```
   ```bash
   gh secret set APPLE_API_ISSUER_ID          # the Issuer ID
   ```
5. Delete the local `.p12` and the base64 copy. Keep the `.p8` somewhere safe, or revoke it and make a new one if it leaks.

### Checking a signed release

```bash
codesign -dv --verbose=2 fix-network 2>&1 | grep -E "Authority=Developer ID Application|flags=.*runtime|Timestamp"
```

Then download the tarball in a browser, unpack it, and run `./fix-network --help`. It should start without a warning.

Notarization can't be stapled to a bare executable. The first launch on each Mac checks the notarization with Apple online.

## Language modules (`language-modules.sh`)

The 25 terse-view grammars not built into zazzled ship as language modules (`macos/rust/zazzle-lang`, one cdylib per Cargo feature), which zazzled downloads on demand and checks against a manifest compiled into it.

- `language-modules.sh OUT [TARGET...]` builds every module into `OUT/NAME.dylib` (macOS, universal with `lipo` when given both Apple targets) or `OUT/NAME.so` (Linux, the host). `LANGUAGES="lua go"` limits it.
- `language-modules.sh --manifest OUT` writes `OUT/manifest`, one `NAME SHA256 FILE` line per module.
- The release builds the modules, signs them on macOS with `sign-and-notarize.sh` (a signed, hardened zazzled only loads libraries signed by the same team), writes the manifest, then builds zazzled with `ZAZZLED_LANGUAGES_MANIFEST` pointing at it, so the hashes are those of the signed files. It uploads the modules and the manifest to `icelander-downloads/zazzle/vX.Y.Z/languages/{macos-universal,linux-x86_64,linux-aarch64}/`, under the tag only: a zazzled accepts only its own release's modules.
- `language-modules.test.sh` builds the Lua module and checks its manifest line; CI runs it with shellcheck.

## cyberspace-generator (`fetch-cyberspace-generator.sh`)

`cyberspace-generator` is built and released in [cyberspace-explorer](https://github.com/david-brandt-the-icelander/cyberspace-explorer) (tags `generator-v*`). `cyberspace-generator.version` pins the release zazzle ships. The release job runs `fetch-cyberspace-generator.sh`, which downloads that release's binaries and checks them against its `SHA256SUMS`; the macOS job merges the two CPU types with `lipo` and signs the result with the other binaries, and each Linux tarball gets the static musl build.

`.github/workflows/bump-cyberspace-generator.yml` checks daily (or on demand) for a newer `generator-v*` release and opens a pull request moving the pin, after checking that release has every binary. It uses `GITHUB_TOKEN` only, so the repository must allow GitHub Actions to create pull requests (Settings → Actions → General); since such pull requests start no workflows, it dispatches `ci.yml` on the branch itself. Merge it, then tag a zazzle release to ship it. `fetch-cyberspace-generator.test.sh` checks the script against a fake release.
