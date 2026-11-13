#!/bin/sh
# Times zazzled refs over the cargo registry's crate sources: cold in a fresh process per run, then warm through a running daemon.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$here/..
cold=${1:-20}
warm=${2:-100}
bin=$root/build
zazzled=${ZAZZLED:-$root/../../rust/target/release/zazzled}
src=""
for d in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/; do
	if [ -d "$d" ]; then
		src=$d
		break
	fi
done
[ -n "$src" ] || { echo "no cargo registry sources; build zazzled first" >&2; exit 1; }
tmp=$(mktemp -d)
dpid=
trap '[ -n "$dpid" ] && kill "$dpid" 2>/dev/null; rm -rf "$tmp"' EXIT
export ZAZZLE_SOCK="$tmp/sock" HOME="$tmp/home"
mkdir -p "$HOME"
cd "$src"
echo "# refs over $(find . -type f | wc -l | tr -d ' ') files ($(du -sk . | cut -f1) KB of crate sources), $(uname -sm)"
for sym in Error Result; do
	SPAWNBENCH_WARMUP=1 "$bin/spawnbench" "$cold" "refs-$sym-cold" "$zazzled" run refs "$sym"
done
"$zazzled" serve --socket "$ZAZZLE_SOCK" &
dpid=$!
i=0
while [ ! -S "$ZAZZLE_SOCK" ] && [ "$i" -lt 100 ]; do
	sleep 0.05
	i=$((i + 1))
done
[ -S "$ZAZZLE_SOCK" ] || { echo "zazzled did not start" >&2; exit 1; }
for sym in Error Result; do
	SPAWNBENCH_WARMUP=3 "$bin/spawnbench" "$warm" "refs-$sym-warm" "$bin/zazzle" refs "$sym"
done
