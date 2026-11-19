#!/usr/bin/env bash
# Pack a release tarball from a flat folder of built files: pack.sh DIST OUT.tar.gz FILE...
#
# The tarball holds the install layout the launcher expects: bin/zazzle, the only command meant for PATH, with
# bin/z as a relative link to it; every other FILE in libexec/zazzle/, where `zazzle TOOL` finds it; and the
# compressed C# indexers (*.gz, macOS) in libexec/indexer/, which the Homebrew formula expands after install.
# bottle.sh and the formula take this layout as it is.
set -euo pipefail

dist=${1:-}
out=${2:-}
if [ ! -d "$dist" ] || [ -z "$out" ] || [ $# -lt 3 ]; then
  echo "usage: $0 DIST OUT.tar.gz FILE...   (FILE names files in DIST; zazzle is required)" >&2
  exit 2
fi
shift 2
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/libexec/zazzle"
launcher=""
for f in "$@"; do
  [ -f "$dist/$f" ] || { echo "missing file: $dist/$f" >&2; exit 1; }
  case "$f" in
    zazzle) cp -p "$dist/$f" "$work/bin/zazzle" && launcher=1 ;;
    *.gz) mkdir -p "$work/libexec/indexer" && cp -p "$dist/$f" "$work/libexec/indexer/$f" ;;
    *) cp -p "$dist/$f" "$work/libexec/zazzle/$f" ;;
  esac
done
[ -n "$launcher" ] || { echo "the launcher, zazzle, is required" >&2; exit 1; }
ln -s zazzle "$work/bin/z"
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac
tar -C "$work" -czf "$out" bin libexec
echo "packed $out"
