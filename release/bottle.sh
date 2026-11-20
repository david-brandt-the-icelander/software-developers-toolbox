#!/usr/bin/env bash
# Build the zazzle Homebrew bottle from the unpacked release tarball (pack.sh): [FORMULA=zazzle-alpha] bottle.sh TAG DIR OUT_DIR
#
# A bottle lets `brew install` pour the binaries without Homebrew's build-from-source checks (such as
# up-to-date Command Line Tools). The binaries are universal and the formula macOS-only, so the bottle is
# tagged `all`. Homebrew requires the archive to hold exactly NAME/VERSION/..., and with a plain root_url
# it downloads NAME-VERSION.all.bottle.tar.gz (one hyphen): the name written here. It holds the keg the formula
# installs: bin/zazzle, the only file brew links, and the tools in libexec/zazzle. The expanded C# indexer is
# not in it: the formula's post_install_steps writes libexec/zazzle/csharp-indexer for this CPU from the two
# compressed ones in libexec/indexer, and links z (outside the keg) when nothing else uses it.
# Needs GNU tar (the release job runs it on Ubuntu).
set -euo pipefail

tag=${1:-}
bin_dir=${2:-}
out=${3:-}
name=${FORMULA:-zazzle}
[[ "$name" =~ ^zazzle(-alpha)?$ ]] || { echo "unknown formula '$name'; use zazzle or zazzle-alpha" >&2; exit 2; }
if ! [[ "$tag" =~ ^v[0-9]+(\.[0-9]+)*(-alpha\.[0-9]+)?$ ]] || [ ! -d "$bin_dir" ] || [ -z "$out" ]; then
  echo "usage: $0 vX.Y.Z DIR OUT_DIR   (DIR: the unpacked tarball, with bin/ and libexec/)" >&2
  exit 2
fi
version=${tag#v}
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

root="$work/$name/$version"
mkdir -p "$root/bin" "$root/libexec/zazzle" "$root/libexec/indexer"
# The launcher is the only command on PATH; `zazzle TOOL` runs each tool from libexec/zazzle.
[ -f "$bin_dir/bin/zazzle" ] || { echo "missing binary: $bin_dir/bin/zazzle" >&2; exit 1; }
install -m 0755 "$bin_dir/bin/zazzle" "$root/bin/zazzle"
for binary in daemon zazz-script-runner fix-network free-disk-space git-tools git-tools-commit git-tools-commit-message-editor ai-agent-rules cyberspace-generator; do
  [ -f "$bin_dir/libexec/zazzle/$binary" ] || { echo "missing binary: $bin_dir/libexec/zazzle/$binary" >&2; exit 1; }
  install -m 0755 "$bin_dir/libexec/zazzle/$binary" "$root/libexec/zazzle/$binary"
done
for arch in arm64 x64; do
  file="csharp-indexer-$arch.gz"
  [ -f "$bin_dir/libexec/indexer/$file" ] || { echo "missing indexer: $bin_dir/libexec/indexer/$file" >&2; exit 1; }
  install -m 0644 "$bin_dir/libexec/indexer/$file" "$root/libexec/indexer/$file"
done
file="$out/$name-$version.all.bottle.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -C "$work" -cf - "$name" | gzip -n -9 >"$file"
echo "$(sha256sum "$file" | cut -d' ' -f1)  $(basename "$file")"
