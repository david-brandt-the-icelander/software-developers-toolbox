#!/usr/bin/env bash
# Tests tools.sh: it builds zazzle-llm for the host and writes a manifest line whose SHA-256 matches the file.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
cd "$here/../rust"
PROFILE=dev "$here/tools.sh" "$out"
[ -x "$out/zazzle-llm" ] || { echo "FAIL: no zazzle-llm"; exit 1; }
"$out/zazzle-llm" --version | grep -q '^zazzle-llm ' || { echo "FAIL: zazzle-llm --version"; exit 1; }
"$here/tools.sh" --manifest "$out"
read -r name sum listed < "$out/manifest"
if command -v sha256sum >/dev/null; then want=$(sha256sum "$out/zazzle-llm" | cut -d' ' -f1); else want=$(shasum -a 256 "$out/zazzle-llm" | cut -d' ' -f1); fi
[ "$name $sum $listed" = "zazzle-llm $want zazzle-llm" ] || { echo "FAIL: manifest line '$name $sum $listed'"; exit 1; }
[ "$(wc -l < "$out/manifest")" -eq 1 ] || { echo "FAIL: manifest has extra lines"; exit 1; }
if TOOLS=nope "$here/tools.sh" --manifest "$out" 2>/dev/null; then echo "FAIL: missing tool accepted"; exit 1; fi
if "$here/tools.sh" 2>/dev/null; then echo "FAIL: no arguments accepted"; exit 1; fi
echo "tools.sh: ok"
