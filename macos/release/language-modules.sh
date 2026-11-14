#!/usr/bin/env bash
# Builds the language modules (one zazzle-lang cdylib per language feature) and writes their manifest, which zazzled embeds and checks downloads against.
#
#   language-modules.sh OUT [TARGET...]   builds every module into OUT/NAME.so (Linux) or OUT/NAME.dylib (macOS, universal
#                                         when given both Apple targets); with no TARGET, for the host. Run in macos/rust.
#   language-modules.sh --manifest OUT    writes OUT/manifest: one `NAME SHA256 FILE` line per module, after any signing
#
# LANGUAGES="lua go" limits the build to those modules (tests use it).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)

# Prints the module names: the zazzle-lang features, or LANGUAGES.
names() {
  if [ -n "${LANGUAGES:-}" ]; then
    # Word splitting is the point: LANGUAGES is a space-separated list.
    # shellcheck disable=SC2086
    printf '%s\n' $LANGUAGES
  else
    sed -n '/^\[features\]/,/^\[/p' "$here/../rust/zazzle-lang/Cargo.toml" | sed -n 's/^\([a-z0-9_]*\) = .*/\1/p'
  fi
}

# Prints a file's SHA-256 in lowercase hex.
sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

if [ "${1:-}" = --manifest ]; then
  [ "$#" -eq 2 ] || { echo "usage: $0 --manifest OUT" >&2; exit 2; }
  out=$2
  : > "$out/manifest.tmp"
  for n in $(names); do
    file=
    for f in "$n.so" "$n.dylib"; do
      if [ -f "$out/$f" ]; then file=$f; fi
    done
    [ -n "$file" ] || { echo "no module file for $n in $out" >&2; exit 1; }
    printf '%s %s %s\n' "$n" "$(sha256 "$out/$file")" "$file" >> "$out/manifest.tmp"
  done
  mv "$out/manifest.tmp" "$out/manifest"
  exit 0
fi

[ "$#" -ge 1 ] || { echo "usage: $0 OUT [TARGET...] | --manifest OUT" >&2; exit 2; }
out=$1
shift
mkdir -p "$out"
case "$(uname -s)" in Darwin) ext=dylib ;; *) ext=so ;; esac
built=target/zazzle-lang
for n in $(names); do
  libs=()
  if [ "$#" -eq 0 ]; then
    cargo build --release --locked -p zazzle-lang --features "$n" --target-dir "$built"
    cp "$built/release/libzazzle_lang.$ext" "$out/.$n.host"
    libs+=("$out/.$n.host")
  else
    for t in "$@"; do
      cargo build --release --locked -p zazzle-lang --features "$n" --target-dir "$built" --target "$t"
      cp "$built/$t/release/libzazzle_lang.$ext" "$out/.$n.$t"
      libs+=("$out/.$n.$t")
    done
  fi
  if [ "${#libs[@]}" -gt 1 ]; then
    lipo -create -output "$out/$n.$ext" "${libs[@]}"
  else
    cp "${libs[0]}" "$out/$n.$ext"
  fi
  rm -f "${libs[@]}"
done
