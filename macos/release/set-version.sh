#!/usr/bin/env bash
# Writes a version into every Rust crate's manifest and its entry in the lock file (no network), so release binaries report the release version.
# usage: set-version.sh VERSION   (run from the repository root)
set -euo pipefail

v=$1
[[ $v =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "not a version: '$v'" >&2; exit 1; }
for manifest in macos/rust/*/Cargo.toml; do
  name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$manifest" | head -1)
  V=$v perl -pi -e 's/^version = "[^"]*"/version = "$ENV{V}"/' "$manifest"
  # The lock file lists each workspace crate as name then version; change that version without resolving anything.
  V=$v N=$name perl -0pi -e 's/(\[\[package\]\]\nname = "\Q$ENV{N}\E"\nversion = ")[^"]*/$1$ENV{V}/' macos/rust/Cargo.lock
done
