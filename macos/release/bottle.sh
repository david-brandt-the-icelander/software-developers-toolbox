#!/usr/bin/env bash
# Build one Homebrew bottle per tool from the release binaries: bottle.sh TAG BIN_DIR OUT_DIR
#
# A bottle lets `brew install` pour the binary without Homebrew's build-from-source checks (such as
# up-to-date Command Line Tools). The binaries are universal and the formulas macOS-only, so each tool
# gets one bottle tagged `all`. Homebrew requires the archive to hold exactly NAME/VERSION/..., and with
# a plain root_url it downloads NAME-VERSION.all.bottle.tar.gz (one hyphen): the names written here.
# Needs GNU tar (the release job runs it on Ubuntu).
set -euo pipefail

tag=${1:-}
bin_dir=${2:-}
out=${3:-}
if ! [[ "$tag" =~ ^v[0-9]+(\.[0-9]+)*$ ]] || [ ! -d "$bin_dir" ] || [ -z "$out" ]; then
  echo "usage: $0 vX.Y.Z BIN_DIR OUT_DIR" >&2
  exit 2
fi
version=${tag#v}
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Pack one formula's binary into NAME/VERSION/bin/BINARY, reproducibly (fixed owner, time and order).
bottle() {
  local name=$1 binary=$2 file
  [ -f "$bin_dir/$binary" ] || { echo "missing binary: $bin_dir/$binary" >&2; exit 1; }
  mkdir -p "$work/$name/$version/bin"
  install -m 0755 "$bin_dir/$binary" "$work/$name/$version/bin/$binary"
  file="$out/$name-$version.all.bottle.tar.gz"
  tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -C "$work" -cf - "$name" | gzip -n -9 >"$file"
  echo "$(sha256sum "$file" | cut -d' ' -f1)  $(basename "$file")"
}

bottle fix-network fix-network
bottle free-disk-space free-disk-space
bottle agent-fs afs
