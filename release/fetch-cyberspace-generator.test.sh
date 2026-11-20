#!/usr/bin/env bash
# Test fetch-cyberspace-generator.sh against a fake release on disk: good checksums pass, a bad one or a missing target fails.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0
tag=$(tr -d '[:space:]' <"$here/cyberspace-generator.version")

# Record a failed expectation.
fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

rel="$tmp/rel/$tag"
mkdir -p "$rel"
for t in a-target b-target; do printf 'bin-%s' "$t" >"$rel/cyberspace-generator-$t"; done
(cd "$rel" && sha256sum cyberspace-generator-* >SHA256SUMS)
export CYBERSPACE_GENERATOR_BASE="file://$tmp/rel"

bash "$here/fetch-cyberspace-generator.sh" "$tmp/out" a-target b-target >/dev/null || fail "good release refused"
[ "$(cat "$tmp/out/cyberspace-generator-b-target")" = "bin-b-target" ] || fail "wrong content"
[ -x "$tmp/out/cyberspace-generator-a-target" ] || fail "not executable"
[ ! -e "$tmp/out/SHA256SUMS" ] || fail "SHA256SUMS left behind"
if bash "$here/fetch-cyberspace-generator.sh" "$tmp/o2" c-target >/dev/null 2>&1; then fail "missing target accepted"; fi
printf 'tampered' >"$rel/cyberspace-generator-a-target"
if bash "$here/fetch-cyberspace-generator.sh" "$tmp/o3" a-target >/dev/null 2>&1; then fail "bad checksum accepted"; fi
if bash "$here/fetch-cyberspace-generator.sh" >/dev/null 2>&1; then fail "no arguments accepted"; fi

[ "$failures" -eq 0 ] || { echo "$failures check(s) failed"; exit 1; }
echo "fetch-cyberspace-generator.sh: all checks passed"
