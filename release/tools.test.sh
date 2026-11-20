#!/usr/bin/env bash
# Tests tools.sh: it builds ai-agent for the host and writes a manifest line whose SHA-256 matches the file.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
cd "$here/../src/macos"
PROFILE=dev "$here/tools.sh" "$out"
[ -x "$out/ai-agent" ] || { echo "FAIL: no ai-agent"; exit 1; }
"$out/ai-agent" --version | grep -q '^ai-agent ' || { echo "FAIL: ai-agent --version"; exit 1; }
"$here/tools.sh" --manifest "$out"
read -r name sum listed < "$out/manifest"
if command -v sha256sum >/dev/null; then want=$(sha256sum "$out/ai-agent" | cut -d' ' -f1); else want=$(shasum -a 256 "$out/ai-agent" | cut -d' ' -f1); fi
[ "$name $sum $listed" = "ai-agent $want ai-agent" ] || { echo "FAIL: manifest line '$name $sum $listed'"; exit 1; }
[ "$(wc -l < "$out/manifest")" -eq 1 ] || { echo "FAIL: manifest has extra lines"; exit 1; }
if TOOLS=nope "$here/tools.sh" --manifest "$out" 2>/dev/null; then echo "FAIL: missing tool accepted"; exit 1; fi
if "$here/tools.sh" 2>/dev/null; then echo "FAIL: no arguments accepted"; exit 1; fi
echo "tools.sh: ok"
