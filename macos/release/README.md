# Release: Homebrew formula and optional signing

On every `v*` tag, `.github/workflows/release.yml` builds universal binaries and the C# indexer and publishes them to a GitHub release, icelander.net/downloads and the Homebrew tap. Two scripts here take part.

## Homebrew formula (`formula.sh`, `bottle.sh`)

The `homebrew` job writes `Formula/zazzle.rb` into the tap repository `zazzcat/homebrew-z` and pushes it to that repo's default branch.

- One formula installs `zazzle`, `zazzled`, `afs`, `fix-network` and `free-disk-space` (universal) and `zazzle-csharp-indexer`. The C# indexer is not universal, so the tarball holds it gzipped for both CPU types (`zazzle-csharp-indexer-arm64.gz`, `-x64.gz`). `bin/zazzle-csharp-indexer` starts as a placeholder so brew links it, and the formula's `post_install_steps` replace it with the binary for this Mac (`brew postinstall zazzle` reruns that).
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
