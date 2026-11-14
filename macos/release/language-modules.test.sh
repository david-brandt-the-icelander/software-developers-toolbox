#!/usr/bin/env bash
# Tests language-modules.sh: it builds one module for the host and writes a manifest line whose SHA-256 matches the file.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
cd "$here/../rust"
LANGUAGES=lua "$here/language-modules.sh" "$out"
case "$(uname -s)" in Darwin) file=lua.dylib ;; *) file=lua.so ;; esac
[ -f "$out/$file" ] || { echo "FAIL: no $file"; exit 1; }
LANGUAGES=lua "$here/language-modules.sh" --manifest "$out"
read -r name sum listed < "$out/manifest"
if command -v sha256sum >/dev/null; then want=$(sha256sum "$out/$file" | cut -d' ' -f1); else want=$(shasum -a 256 "$out/$file" | cut -d' ' -f1); fi
[ "$name $sum $listed" = "lua $want $file" ] || { echo "FAIL: manifest line '$name $sum $listed'"; exit 1; }
[ "$(wc -l < "$out/manifest")" -eq 1 ] || { echo "FAIL: manifest has extra lines"; exit 1; }
if LANGUAGES=go "$here/language-modules.sh" --manifest "$out" 2>/dev/null; then echo "FAIL: missing module accepted"; exit 1; fi
if "$here/language-modules.sh" 2>/dev/null; then echo "FAIL: no arguments accepted"; exit 1; fi
names=$(sed -n '/^\[features\]/,/^\[/p' zazzle-lang/Cargo.toml | sed -n 's/^\([a-z0-9_]*\) = .*/\1/p' | wc -l)
[ "$names" -eq 25 ] || { echo "FAIL: expected 25 module features, found $names"; exit 1; }
echo "language-modules.sh: ok"
