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
[ "$(run branch main patch 1.2.19)" = $'tag=v1.2.19\nversion=1.2.19\npublish=true' ] || fail "exact version"
[ "$(run branch main none v2.5.0)" = $'tag=v2.5.0\nversion=2.5.0\npublish=true' ] || fail "exact version with v"
[ "$(run tag v3.4.5 none 9.9.9)" = $'tag=v3.4.5\nversion=3.4.5\npublish=true' ] || fail "pushed tag wins over version"
if run branch main patch 1.2 2>/dev/null; then fail "bad exact version accepted"; fi
if run branch main patch 1.10.0 2>/dev/null; then fail "existing tag accepted as exact version"; fi
if run tag release-1 none 2>/dev/null; then fail "bad tag accepted"; fi
if run branch main huge 2>/dev/null; then fail "bad bump accepted"; fi
# The alpha channel: the next alpha number for the version, the stable release on or off, and alpha tags ignored by bumps.
[ "$(run branch main patch "" both)" = $'tag=v1.10.1\nversion=1.10.1\npublish=true\nalpha_tag=v1.10.1-alpha.1\nalpha_version=1.10.1-alpha.1\nstable=true' ] || fail "both channels"
git -C "$tmp" tag v1.10.0-alpha.1
git -C "$tmp" tag v1.10.0-alpha.2
[ "$(run branch main none 1.10.0 alpha)" = $'tag=v1.10.0\nversion=1.10.0\npublish=false\nalpha_tag=v1.10.0-alpha.3\nalpha_version=1.10.0-alpha.3\nstable=false' ] || fail "alpha of a released version"
git -C "$tmp" tag v1.10.1-alpha.1
[ "$(run branch main patch)" = $'tag=v1.10.1\nversion=1.10.1\npublish=true' ] || fail "alpha tags are not the latest release"
[ "$(run branch main none "" both)" = $'tag=\nversion=0.0.0-dev\npublish=false\nalpha_tag=\nalpha_version=\nstable=true' ] || fail "build only with channel"
if run branch main patch "" beta 2>/dev/null; then fail "bad channel accepted"; fi
echo "version ok"
