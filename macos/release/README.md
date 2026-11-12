# Release: Homebrew formulas and optional signing

On every `v*` tag, `.github/workflows/release.yml` builds universal binaries and publishes them to a GitHub release and icelander.net/downloads. Two scripts here take part.

## Homebrew formulas (`formula.sh`)

The `homebrew` job runs `formula.sh TAG SHA256 Formula` and commits `Formula/fix-network.rb`, `Formula/free-disk-space.rb` and `Formula/agent-fs.rb` to `main`.

- Each formula installs one binary from the release tarball.
- Each takes the release tag as its version, so every release is an upgrade for all three.
- Homebrew doesn't quarantine what a formula installs, so unsigned binaries open without a Gatekeeper prompt.
- Users install with:
  ```bash
  brew tap david-brandt-the-icelander/toolbox https://github.com/david-brandt-the-icelander/software-developers-toolbox
  ```
  ```bash
  brew install david-brandt-the-icelander/toolbox/fix-network
  ```
- The job pushes with the workflow's own token. If `main` gets branch protection, allow GitHub Actions to push, or change the job to open a pull request.
- `formula.test.sh` checks the generated Ruby against a stand-in Homebrew `Formula` class: every DSL value, the installed binary, the `--version` name each crate prints, and that `release.yml` builds each binary. CI runs it.

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
