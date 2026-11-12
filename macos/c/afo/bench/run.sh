#!/bin/sh
# Starts the daemon on a temp socket and prints spawn latency for the invoker against common tools.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$here/..
n=${1:-3000}
bin=$root/build
afod=${AFOD:-$root/../../rust/target/release/afod}
tmp=$(mktemp -d)
dpid=
trap '[ -n "$dpid" ] && kill "$dpid" 2>/dev/null; rm -rf "$tmp"' EXIT
export AFO_SOCK=$tmp/sock

echo "# $(uname -sm) $(sysctl -n machdep.cpu.brand_string 2>/dev/null || grep -m1 'model name' /proc/cpuinfo || true)"
"$afod" serve --socket "$AFO_SOCK" &
dpid=$!
i=0
while [ ! -S "$AFO_SOCK" ] && [ "$i" -lt 100 ]; do
	sleep 0.05
	i=$((i + 1))
done
[ -S "$AFO_SOCK" ] || { echo "afod did not start" >&2; exit 1; }

mkdir -p "$tmp/work/src"
for k in 1 2 3 4 5 6 7 8 9 10; do
	printf 'fn alpha%s() {}\nlet needle = %s;\n' "$k" "$k" > "$tmp/work/src/f$k.rs"
done
cd "$tmp/work"
sb=$bin/spawnbench
"$sb" "$n" exit0 "$bin/exit0"
"$sb" "$n" hello_libc "$bin/hello_libc"
"$sb" "$n" afo-version "$bin/afo" version
"$sb" "$n" afo-find "$bin/afo" find needle
"$sb" "$n" grep-rn /usr/bin/grep -rn needle src
"$sb" "$n" afo-read "$bin/afo" read src/f1.rs
"$sb" "$n" cat /bin/cat src/f1.rs
