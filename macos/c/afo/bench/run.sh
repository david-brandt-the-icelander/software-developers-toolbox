#!/bin/sh
# Starts the spike daemon on a temp socket and prints spawn latency for each invoker variant.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$here/..
n=${1:-5000}
bin=$root/build
afod=${AFOD:-$root/../../rust/target/release/afod}
tmp=$(mktemp -d)
dpid=
trap '[ -n "$dpid" ] && kill "$dpid" 2>/dev/null; rm -rf "$tmp"' EXIT
export AFO_SOCK=$tmp/sock

echo "# $(uname -sm) $(sysctl -n machdep.cpu.brand_string 2>/dev/null || grep -m1 'model name' /proc/cpuinfo || true)"
"$afod" &
dpid=$!
i=0
while [ ! -S "$AFO_SOCK" ] && [ "$i" -lt 100 ]; do
	sleep 0.05
	i=$((i + 1))
done
[ -S "$AFO_SOCK" ] || { echo "afod did not start" >&2; exit 1; }

sb=$bin/spawnbench
"$sb" "$n" exit0 "$bin/exit0"
"$sb" "$n" hello_libc "$bin/hello_libc"
"$sb" "$n" /bin/true /bin/true
"$sb" "$n" afo-fd-16 "$bin/afo" 16
"$sb" "$n" afo-fd-4096 "$bin/afo" 4096
"$sb" "$n" afo-relay-16 "$bin/afo-relay" 16
"$sb" "$n" afo-relay-4096 "$bin/afo-relay" 4096
