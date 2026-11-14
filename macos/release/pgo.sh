#!/bin/sh
# Trains a clang profile for zazzled's C code (tree-sitter) on crate sources and prints PGO_CFLAGS=... for the optimized build.
# Usage: PGO_LANGUAGES=DIR sh macos/release/pgo.sh [CORPUS_DIR] >> "$GITHUB_ENV"; then build with CC=clang and CFLAGS="$PGO_CFLAGS".
# PGO_LANGUAGES is a directory of built language modules with their manifest (language-modules.sh): zazzled has no grammars of its own, so training loads them from there.
# Progress goes to stderr. The default corpus is the cargo registry sources that building zazzled downloads.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
rust=$here/../rust
out=${PGO_DIR:-$rust/target/pgo}
corpus=${1:-}
if [ -z "$corpus" ]; then
	for d in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/; do
		if [ -d "$d" ]; then
			corpus=$d
			break
		fi
	done
fi
if [ -z "$corpus" ] || [ ! -d "$corpus" ]; then
	echo "pgo.sh: no training corpus; build zazzled once first" >&2
	exit 1
fi

# Runs llvm-profdata from the same toolchain as clang (Xcode's on macOS).
profdata() {
	if [ "$(uname -s)" = Darwin ]; then
		xcrun llvm-profdata "$@"
	else
		llvm-profdata "$@"
	fi
}

# Prints the rustc flags that link clang's profile runtime into the instrumented binary.
runtime_flags() {
	if [ "$(uname -s)" = Darwin ]; then
		echo "-C link-arg=-fprofile-instr-generate"
		return
	fi
	# rustc links with -nodefaultlibs: force in the runtime hook that writes the profile at exit, name the archive, and put libc (for atexit) after it.
	dir=$(clang -print-runtime-dir)
	for rt in "$dir/libclang_rt.profile-$(uname -m).a" "$dir/libclang_rt.profile.a"; do
		[ -f "$rt" ] && { echo "-C link-arg=-Wl,-u,__llvm_profile_runtime -C link-arg=$rt -C link-arg=-lc"; return; }
	done
	echo "pgo.sh: no clang profile runtime in $dir (install libclang-rt-<version>-dev)" >&2
	exit 1
}

# An explicit --target keeps RUSTFLAGS off build scripts, which must not link the profile runtime.
host=$(rustc -vV | sed -n 's/^host: //p')
rm -rf "$out"
mkdir -p "$out/raw" "$out/home"
echo "pgo.sh: building instrumented zazzled for $host" >&2
rtflags=$(runtime_flags)
languages=${PGO_LANGUAGES:-}
if [ -z "$languages" ] || [ ! -f "$languages/manifest" ]; then
	echo "pgo.sh: set PGO_LANGUAGES to a directory of language modules with their manifest" >&2
	exit 1
fi
languages=$(cd "$languages" && pwd)
(cd "$rust" && CC=clang CFLAGS="-fprofile-instr-generate" RUSTFLAGS="$rtflags" ZAZZLED_LANGUAGES_MANIFEST="$languages/manifest" \
	cargo build --release --locked -p zazzled --target "$host" --target-dir "$out/build" >&2)
bin=$out/build/$host/release/zazzled

echo "pgo.sh: training on $corpus" >&2
export LLVM_PROFILE_FILE="$out/raw/zazzled-%p-%m.profraw" HOME="$out/home" ZAZZLE_LANGUAGES_DIR="$languages"
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
