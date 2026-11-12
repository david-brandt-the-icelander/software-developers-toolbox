#!/usr/bin/env bash
# Test sign-and-notarize.sh against recording stand-ins for security, codesign, xcrun and ditto (runs on Linux and macOS).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/sign-and-notarize.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0

# Write the stand-in tools; each appends its command line to $CALLS.
mkdir "$tmp/bin"
for tool in security codesign xcrun ditto; do
  cat >"$tmp/bin/$tool" <<'EOF'
#!/usr/bin/env bash
echo "$(basename "$0") $*" >>"$CALLS"
case "$(basename "$0") $1 $2" in
  "security find-identity"*) printf '  1) ABC123 "%s"\n     1 valid identities found\n' "$FAKE_IDENTITY" ;;
  "xcrun notarytool submit")
    [ "$FAKE_NOTARY" = crash ] && { echo "Error: HTTP status code: 401" >&2; exit 1; }
    printf '{"id":"sub-42","status":"%s","message":"done"}\n' "$FAKE_NOTARY" ;;
  "xcrun notarytool log") echo '{"issues":[{"message":"The binary is not signed with a valid Developer ID certificate."}]}' ;;
  "ditto "*) touch "${@: -1}" ;;
esac
EOF
  chmod +x "$tmp/bin/$tool"
done

# Run the script in a fresh directory with the given extra environment; sets $rc, $out and $calls.
run() {
  mkdir -p "$tmp/dist"
  printf 'binary' >"$tmp/dist/fix-network"
  printf 'binary' >"$tmp/dist/afs"
  : >"$tmp/calls"
  set +e
  out=$(env -i PATH="$tmp/bin:$PATH" HOME="$tmp" CALLS="$tmp/calls" \
    FAKE_IDENTITY="${FAKE_IDENTITY:-Developer ID Application: Test Person (TEAM123456)}" \
    FAKE_NOTARY="${FAKE_NOTARY:-Accepted}" "$@" bash "$script" "$tmp/dist" fix-network afs 2>&1)
  rc=$?
  set -e
  calls=$(cat "$tmp/calls")
}

# Record a failed expectation.
fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

# Expect an exit code.
expect_rc() {
  [ "$rc" -eq "$1" ] || fail "$2: exit $rc, wanted $1. Output: $out"
}

# Expect text in a haystack.
expect_in() {
  case "$2" in *"$1"*) ;; *) fail "$3: missing '$1'" ;; esac
}

# Expect text absent from a haystack.
expect_not_in() {
  case "$2" in *"$1"*) fail "$3: unexpected '$1'" ;; esac
}

secrets=(MACOS_CERTIFICATE_P12="$(printf 'p12-bytes' | base64)" MACOS_CERTIFICATE_PASSWORD=pw
  APPLE_API_KEY_P8="-----BEGIN PRIVATE KEY-----" APPLE_API_KEY_ID=KEY123 APPLE_API_ISSUER_ID=issuer-uuid)

# 1. No secrets on a manual run: warn, touch nothing, succeed.
run
expect_rc 0 "no secrets"
expect_in "NOT signed or notarized" "$out" "no secrets"
expect_in "APPLE_API_ISSUER_ID" "$out" "no secrets lists what is missing"
[ -z "$calls" ] || fail "no secrets: tools ran: $calls"

# 2. No secrets on a tag: fail before releasing unsigned binaries.
run REQUIRE_SIGNING=true MACOS_CERTIFICATE_P12=x
expect_rc 1 "required"
expect_in "::error::cannot sign the release" "$out" "required"
expect_not_in "MACOS_CERTIFICATE_P12 " "$out" "required lists only missing secrets"

# 3. Everything set and Apple accepts: both binaries signed with hardened runtime, notarized, keychain removed.
run "${secrets[@]}"
expect_rc 0 "happy path"
expect_in "signed and notarized: fix-network afs (submission sub-42)" "$out" "happy path"
expect_in "codesign --force --options runtime --timestamp --keychain" "$calls" "hardened runtime and timestamp"
expect_in "--sign Developer ID Application: Test Person (TEAM123456) $tmp/dist/fix-network" "$calls" "signs fix-network"
expect_in "$tmp/dist/afs" "$calls" "signs afs"
expect_in "codesign --verify --strict --verbose=2 $tmp/dist/afs" "$calls" "verifies"
expect_in "security import" "$calls" "imports the certificate"
expect_in "-P pw -T /usr/bin/codesign" "$calls" "passes the p12 password"
expect_in "xcrun notarytool submit" "$calls" "submits"
expect_in "--key-id KEY123 --issuer issuer-uuid --wait --timeout 30m" "$calls" "uses the API key and waits"
expect_in "security delete-keychain" "$calls" "removes the keychain"
expect_not_in "notarytool log" "$calls" "no log on success"
[ "$(grep -c '^codesign --force' "$tmp/calls")" -eq 2 ] || fail "signs exactly two binaries"

# 4. Apple rejects the submission: fail, print Apple's log, still remove the keychain.
FAKE_NOTARY=Invalid run "${secrets[@]}"
expect_rc 1 "rejected"
expect_in "::error::notarization Invalid (submission sub-42)" "$out" "rejected"
expect_in "not signed with a valid Developer ID" "$out" "rejected shows Apple's log"
expect_in "security delete-keychain" "$calls" "rejected removes the keychain"

# 5. notarytool itself fails (bad API key): fail without a submission id.
FAKE_NOTARY=crash run "${secrets[@]}"
expect_rc 1 "notarytool error"
expect_in "notarization failed (submission none)" "$out" "notarytool error"
expect_not_in "notarytool log" "$calls" "no log without a submission"

# 6. The certificate is not a Developer ID Application one: fail before signing anything.
FAKE_IDENTITY="Apple Development: Test Person (TEAM123456)" run "${secrets[@]}"
expect_rc 1 "wrong certificate"
expect_in "not a valid Developer ID Application certificate" "$out" "wrong certificate"
expect_not_in "codesign --force" "$calls" "wrong certificate signs nothing"

# 7. Missing arguments: usage error.
set +e
out=$(bash "$script" "$tmp/dist" 2>&1)
rc=$?
set -e
expect_rc 2 "usage"
expect_in "usage:" "$out" "usage"

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "sign-and-notarize.sh: all checks passed"
