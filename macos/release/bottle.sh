#!/usr/bin/env bash
# Build the zazzle Homebrew bottle from the release files: bottle.sh TAG BIN_DIR OUT_DIR
#
# A bottle lets `brew install` pour the binaries without Homebrew's build-from-source checks (such as
# up-to-date Command Line Tools). The binaries are universal and the formula macOS-only, so the bottle is
# tagged `all`. Homebrew requires the archive to hold exactly NAME/VERSION/..., and with a plain root_url
# it downloads NAME-VERSION.all.bottle.tar.gz (one hyphen): the name written here. The expanded C# indexer
# is not in it: bin/zazzle-csharp-indexer is a placeholder, so brew links it when pouring, and the formula's
# post_install_steps replaces it with the file for this CPU from the two compressed ones in libexec/indexer.
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

root="$work/zazzle/$version"
mkdir -p "$root/bin" "$root/libexec/indexer"
for binary in zazzle zazz zazzled zazz-worker afs fix-network free-disk-space; do
  [ -f "$bin_dir/$binary" ] || { echo "missing binary: $bin_dir/$binary" >&2; exit 1; }
  install -m 0755 "$bin_dir/$binary" "$root/bin/$binary"
done
for arch in arm64 x64; do
  file="zazzle-csharp-indexer-$arch.gz"
  [ -f "$bin_dir/$file" ] || { echo "missing indexer: $bin_dir/$file" >&2; exit 1; }
  install -m 0644 "$bin_dir/$file" "$root/libexec/indexer/$file"
done
printf '#!/bin/sh\necho "zazzle-csharp-indexer is not set up yet; run: brew postinstall zazzle" >&2\nexit 1\n' \
  >"$root/bin/zazzle-csharp-indexer"
chmod 0755 "$root/bin/zazzle-csharp-indexer"
file="$out/zazzle-$version.all.bottle.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -C "$work" -cf - zazzle | gzip -n -9 >"$file"
echo "$(sha256sum "$file" | cut -d' ' -f1)  $(basename "$file")"
