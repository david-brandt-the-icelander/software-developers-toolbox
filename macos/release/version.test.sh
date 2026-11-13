#!/usr/bin/env bash
# Tests version.sh against a throwaway repository with release tags.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

# Runs version.sh in the throwaway repository.
run() { (cd "$tmp" && bash "$here/version.sh" "$@"); }

git -C "$tmp" init -q
git -C "$tmp" -c user.name=t -c user.email=t@t commit -q --allow-empty -m one
[ "$(run branch main patch)" = $'tag=v0.0.1\nversion=0.0.1\npublish=true' ] || fail "first patch"
for t in v1.2.9 v1.2.10 v1.10.0 v0.9.9 notes; do git -C "$tmp" tag "$t"; done
[ "$(run branch main patch)" = $'tag=v1.10.1\nversion=1.10.1\npublish=true' ] || fail "patch after v1.10.0"
[ "$(run branch main minor)" = $'tag=v1.11.0\nversion=1.11.0\npublish=true' ] || fail "minor"
[ "$(run branch main major)" = $'tag=v2.0.0\nversion=2.0.0\npublish=true' ] || fail "major"
[ "$(run branch main none)" = $'tag=\nversion=0.0.0-dev\npublish=false' ] || fail "build only"
[ "$(run tag v3.4.5 none)" = $'tag=v3.4.5\nversion=3.4.5\npublish=true' ] || fail "pushed tag"
if run tag release-1 none 2>/dev/null; then fail "bad tag accepted"; fi
if run branch main huge 2>/dev/null; then fail "bad bump accepted"; fi
echo "version ok"
