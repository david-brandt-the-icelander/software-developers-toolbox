# Release: Homebrew formulas and optional signing

On every `v*` tag, `.github/workflows/release.yml` builds universal binaries and publishes them to a GitHub release and icelander.net/downloads. Two scripts here take part.

## Homebrew formulas (`formula.sh`)

The `homebrew` job runs `formula.sh TAG SHA256 Formula` and commits `Formula/fix-network.rb`, `Formula/free-disk-space.rb` and `Formula/agent-fs.rb` to `main`.

- Each formula installs one binary. `bottle.sh` packs each binary as a Homebrew bottle (`<name>-<version>.all.bottle.tar.gz`, one per tool, because the binaries are universal) and the job uploads it to the GitHub release. The formula's `bottle` block points there, so `brew install` pours it without Homebrew's build-from-source checks, which demand up-to-date Command Line Tools. The release tarball stays the formula's source fallback.
- Each takes the release tag as its version, so every release is an upgrade for all three.
- Homebrew doesn't quarantine what a formula installs, so unsigned binaries open without a Gatekeeper prompt.
- Users install with:
  ```bash
  brew tap david-brandt-the-icelander/toolbox https://github.com/david-brandt-the-icelander/software-developers-toolbox
  ```
  ```bash
  brew install david-brandt-the-icelander/toolbox/fix-network
  ```
- `main` only accepts pull requests (a repository rule), so the job publishes the formulas in one of two ways:
  - **Pull request (the default).** It pushes a `homebrew/<tag>` branch and opens a PR. Merge the PR, and `brew upgrade` sees the release. This needs Settings → Actions → General → "Allow GitHub Actions to create and approve pull requests".
  - **Direct to `main` (fully automatic).** Create an SSH key pair:
    ```bash
    ssh-keygen -t ed25519 -N "" -f homebrew_deploy_key
    ```
    Then:
    - add `homebrew_deploy_key.pub` as a deploy key with write access (Settings → Deploy keys);
    - add that deploy key to the bypass list of the ruleset on `main` (Settings → Rules);
    - store the private key as the secret `HOMEBREW_DEPLOY_KEY`.
- `formula.test.sh` checks the generated Ruby against a stand-in Homebrew `Formula` class:
  - every DSL value, the installed binary, and the `--version` name each crate prints;
  - that `release.yml` builds each binary;
  - the bottle block: `root_url`, the `all` tag, `:any_skip_relocation`, and a checksum matching the bottle;
  - that each bottle exists under the name Homebrew downloads (`Bottle::Filename#url_encode`);
  - that each bottle holds exactly `NAME/VERSION/bin/BINARY` (Homebrew's `Bottle#extract_snapshot` check), mode 0755, and is reproducible.

  CI runs it.

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
