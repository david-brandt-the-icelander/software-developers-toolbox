#!/usr/bin/env bash
# Download the pinned cyberspace-generator release for some Rust targets and check its checksums: fetch-cyberspace-generator.sh OUT_DIR TARGET...
#
# cyberspace-generator is built in david-brandt-the-icelander/cyberspace-explorer; cyberspace-generator.version
# names the release tag zazzle ships (the bump-cyberspace-generator workflow moves it). The repository is
# public, so no token is needed. Writes OUT_DIR/cyberspace-generator-TARGET, mode 0755.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out=${1:-}
shift || true
if [ -z "$out" ] || [ $# -eq 0 ]; then
  echo "usage: $0 OUT_DIR TARGET..." >&2
  exit 2
fi
tag=$(tr -d '[:space:]' <"$here/cyberspace-generator.version")
[[ "$tag" =~ ^generator-v[0-9]+(\.[0-9]+)*$ ]] || { echo "bad tag in cyberspace-generator.version: $tag" >&2; exit 2; }
base="${CYBERSPACE_GENERATOR_BASE:-https://github.com/david-brandt-the-icelander/cyberspace-explorer/releases/download}/$tag"
mkdir -p "$out"
curl -fsSL --retry 3 -o "$out/SHA256SUMS" "$base/SHA256SUMS"
for target in "$@"; do
  name="cyberspace-generator-$target"
  curl -fsSL --retry 3 -o "$out/$name" "$base/$name"
  want=$(awk -v n="$name" '$2 == n { print $1 }' "$out/SHA256SUMS")
  [ -n "$want" ] || { echo "$name is not in SHA256SUMS of $tag" >&2; exit 1; }
  got=$( (sha256sum "$out/$name" 2>/dev/null || shasum -a 256 "$out/$name") | cut -d' ' -f1)
  [ "$want" = "$got" ] || { echo "checksum mismatch for $name" >&2; exit 1; }
  chmod 0755 "$out/$name"
done
rm -f "$out/SHA256SUMS"
echo "fetched cyberspace-generator $tag for $*"
