#!/bin/sh
# Trains a clang profile for zazzled's C code (tree-sitter) on crate sources and prints PGO_CFLAGS=... for the optimized build.
# Usage: sh macos/release/pgo.sh [CORPUS_DIR] >> "$GITHUB_ENV"; then build with CC=clang and CFLAGS="$PGO_CFLAGS".
# Progress goes to stderr. The default corpus is the cargo registry sources that building zazzled downloads.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
rust=$here/../rust
out=${PGO_DIR:-$rust/target/pgo}
corpus=${1:-$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/ 2>/dev/null | head -1)}
[ -n "$corpus" ] && [ -d "$corpus" ] || { echo "pgo.sh: no training corpus; build zazzled once first" >&2; exit 1; }

# Runs llvm-profdata from the same toolchain as clang (Xcode's on macOS).
profdata() {
	if [ "$(uname -s)" = Darwin ]; then
		xcrun llvm-profdata "$@"
	else
		llvm-profdata "$@"
	fi
}

# Links through clang so the instrumented binary gets clang's profile runtime (Linux defaults to gcc).
linker=""
[ "$(uname -s)" = Darwin ] || linker="-C linker=clang"

# An explicit --target keeps RUSTFLAGS off build scripts, which must not link the profile runtime.
host=$(rustc -vV | sed -n 's/^host: //p')
rm -rf "$out"
mkdir -p "$out/raw" "$out/home"
echo "pgo.sh: building instrumented zazzled for $host" >&2
(cd "$rust" && CC=clang CFLAGS="-fprofile-instr-generate" RUSTFLAGS="$linker -C link-arg=-fprofile-instr-generate" \
	cargo build --release --locked -p zazzled --target "$host" --target-dir "$out/build" >&2)
bin=$out/build/$host/release/zazzled

echo "pgo.sh: training on $corpus" >&2
export LLVM_PROFILE_FILE="$out/raw/zazzled-%p-%m.profraw" HOME="$out/home"
cd "$corpus"
find . -type f \( -name '*.rs' -o -name '*.c' -o -name '*.h' -o -name '*.py' -o -name '*.js' -o -name '*.ts' -o -name '*.tsx' \) \
	-size -1024k -print0 | xargs -0 -n 200 "$bin" run outline >/dev/null 2>&1 || true
for sym in Error Result new parse; do
	"$bin" run refs "$sym" >/dev/null 2>&1 || true
done
ls "$out"/raw/*.profraw >/dev/null 2>&1 || { echo "pgo.sh: training wrote no profile" >&2; exit 1; }
profdata merge -o "$out/zazzled.profdata" "$out"/raw/*.profraw >&2
echo "pgo.sh: profile $out/zazzled.profdata" >&2
echo "PGO_CFLAGS=-fprofile-instr-use=$out/zazzled.profdata -Wno-profile-instr-unprofiled -Wno-profile-instr-out-of-date"
