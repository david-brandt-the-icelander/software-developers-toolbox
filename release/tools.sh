#!/usr/bin/env bash
# Builds the JIT tools (programs daemon downloads on first use instead of shipping them in the tarball: ai-agent) and writes their manifest, which daemon embeds and checks downloads against.
#
#   tools.sh OUT [TARGET...]   builds each tool into OUT/NAME, universal when given both Apple targets (Metal on Apple
#                              Silicon); with no TARGET, for the host. Run in src/macos.
#   tools.sh --manifest OUT    writes OUT/manifest: one `NAME SHA256 FILE` line per tool, after any signing
#
# TOOLS="ai-agent" limits the build to those tools; PROFILE=dev builds unoptimized (tests use both).
set -euo pipefail

# Prints the tool names: TOOLS, or every JIT tool.
names() {
  # Word splitting is the point: TOOLS is a space-separated list.
  # shellcheck disable=SC2086
  printf '%s\n' ${TOOLS:-ai-agent}
}

# Prints a file's SHA-256 in lowercase hex.
sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Prints the cargo feature flags for a tool on a target: Metal for ai-agent on Apple Silicon.
features() {
  case "$1:$2" in
    ai-agent:aarch64-apple-darwin) echo "--features metal" ;;
    ai-agent:host) if [ "$(uname -s)-$(uname -m)" = Darwin-arm64 ]; then echo "--features metal"; fi ;;
  esac
}

if [ "${1:-}" = --manifest ]; then
  [ "$#" -eq 2 ] || { echo "usage: $0 --manifest OUT" >&2; exit 2; }
  out=$2
  : > "$out/manifest.tmp"
  for n in $(names); do
    [ -f "$out/$n" ] || { echo "no tool file for $n in $out" >&2; exit 1; }
    printf '%s %s %s\n' "$n" "$(sha256 "$out/$n")" "$n" >> "$out/manifest.tmp"
  done
  mv "$out/manifest.tmp" "$out/manifest"
  exit 0
fi

[ "$#" -ge 1 ] || { echo "usage: $0 OUT [TARGET...] | --manifest OUT" >&2; exit 2; }
out=$1
shift
mkdir -p "$out"
profile=${PROFILE:-release}
dir=$profile
[ "$profile" = dev ] && dir=debug
for n in $(names); do
  bins=()
  if [ "$#" -eq 0 ]; then
    # shellcheck disable=SC2046
    cargo build --profile "$profile" --locked -p "$n" $(features "$n" host)
    cp "target/$dir/$n" "$out/.$n.host"
    bins+=("$out/.$n.host")
  else
    for t in "$@"; do
      # shellcheck disable=SC2046
      cargo build --profile "$profile" --locked -p "$n" --target "$t" $(features "$n" "$t")
      cp "target/$t/$dir/$n" "$out/.$n.$t"
      bins+=("$out/.$n.$t")
    done
  fi
  if [ "${#bins[@]}" -gt 1 ]; then
    lipo -create -output "$out/$n" "${bins[@]}"
  else
    cp "${bins[0]}" "$out/$n"
  fi
  rm -f "${bins[@]}"
done
