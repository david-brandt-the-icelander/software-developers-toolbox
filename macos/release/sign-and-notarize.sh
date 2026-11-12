#!/usr/bin/env bash
# Sign release binaries with a Developer ID (hardened runtime, timestamp) and notarize them with Apple.
#
#   sign-and-notarize.sh DIR BINARY...     signs DIR/BINARY in place, then notarizes them together
#
# Credentials come from the environment (GitHub secrets in release.yml):
#   MACOS_CERTIFICATE_P12        base64 of the "Developer ID Application" certificate exported as .p12
#   MACOS_CERTIFICATE_PASSWORD   the .p12 export password
#   APPLE_API_KEY_P8             contents of the App Store Connect API key (AuthKey_XXXX.p8)
#   APPLE_API_KEY_ID             that key's ID
#   APPLE_API_ISSUER_ID          the issuer ID shown above the keys in App Store Connect
# Without them it skips with a warning, unless REQUIRE_SIGNING=true (the repository variable), where it fails.
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 DIR BINARY..." >&2
  exit 2
fi
dir=$1
shift

missing=()
for name in MACOS_CERTIFICATE_P12 MACOS_CERTIFICATE_PASSWORD APPLE_API_KEY_P8 APPLE_API_KEY_ID APPLE_API_ISSUER_ID; do
  [ -n "${!name:-}" ] || missing+=("$name")
done
if [ "${#missing[@]}" -gt 0 ]; then
  if [ "${REQUIRE_SIGNING:-false}" = true ]; then
    echo "::error::cannot sign the release, missing secrets: ${missing[*]}"
    exit 1
  fi
  echo "::warning::binaries are NOT signed or notarized, missing secrets: ${missing[*]}"
  exit 0
fi

work=$(mktemp -d)
keychain="$work/signing.keychain-db"
keychain_password=$(openssl rand -hex 24)
# Remove the temporary keychain and every decoded secret, whatever happens.
cleanup() {
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

# A temporary keychain holding only the Developer ID certificate, usable by codesign without a prompt.
printf '%s' "$MACOS_CERTIFICATE_P12" | base64 --decode >"$work/certificate.p12"
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$work/certificate.p12" -k "$keychain" -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_password" "$keychain" >/dev/null
identity=$(security find-identity -v -p codesigning "$keychain" | awk -F'"' '/Developer ID Application/ { print $2; exit }')
if [ -z "$identity" ]; then
  echo "::error::the certificate in MACOS_CERTIFICATE_P12 is not a valid Developer ID Application certificate"
  exit 1
fi
echo "signing with: $identity"

mkdir "$work/notarize"
for bin in "$@"; do
  codesign --force --options runtime --timestamp --keychain "$keychain" --sign "$identity" "$dir/$bin"
  codesign --verify --strict --verbose=2 "$dir/$bin"
  cp "$dir/$bin" "$work/notarize/"
done

# Bare executables can't carry a stapled ticket; Gatekeeper looks the notarization up online on first run.
ditto -c -k --keepParent "$work/notarize" "$work/notarize.zip"
printf '%s\n' "$APPLE_API_KEY_P8" >"$work/AuthKey.p8"
auth=(--key "$work/AuthKey.p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID")
xcrun notarytool submit "$work/notarize.zip" "${auth[@]}" --wait --timeout 30m --output-format json >"$work/result.json" || true
status=$(sed -n 's/.*"status" *: *"\([^"]*\)".*/\1/p' "$work/result.json")
id=$(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' "$work/result.json")
if [ "$status" != "Accepted" ]; then
  echo "::error::notarization ${status:-failed} (submission ${id:-none})"
  cat "$work/result.json"
  if [ -n "$id" ]; then
    xcrun notarytool log "$id" "${auth[@]}" || true
  fi
  exit 1
fi
echo "signed and notarized: $* (submission $id)"
