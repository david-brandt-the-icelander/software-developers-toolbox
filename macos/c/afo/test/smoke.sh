#!/bin/sh
# Checks that both invoker variants get the daemon's payload and that a missing daemon fails cleanly.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$here/..
afod=${AFOD:-$root/../../rust/target/release/afod}
tmp=$(mktemp -d)
dpid=
trap '[ -n "$dpid" ] && kill "$dpid" 2>/dev/null; rm -rf "$tmp"' EXIT

AFO_SOCK=$tmp/none "$root/build/afo" 5 2>"$tmp/err" && { echo "expected failure without daemon" >&2; exit 1; }
grep -q "daemon not running" "$tmp/err"

export AFO_SOCK=$tmp/sock
"$afod" &
dpid=$!
i=0
while [ ! -S "$AFO_SOCK" ] && [ "$i" -lt 100 ]; do
	sleep 0.05
	i=$((i + 1))
done

for v in afo afo-relay; do
	out=$("$root/build/$v" 5)
	[ "$out" = "xxxx" ] || { echo "$v: unexpected output '$out'" >&2; exit 1; }
done
echo "smoke ok"
