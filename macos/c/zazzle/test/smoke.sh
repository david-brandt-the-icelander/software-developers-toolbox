#!/bin/sh
# End-to-end check of the invoker against the real daemon: output, status, errors and autostart.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$here/..
zazzle=$root/build/zazzle
zazzled=${ZAZZLED:-$root/../../rust/target/release/zazzled}
# Absolute, because the checks below cd into a work directory and autostart runs it from there.
case $zazzled in /*) ;; *) zazzled=$(pwd)/$zazzled ;; esac
tmp=$(mktemp -d)
dpid=
cleanup() {
	[ -n "$dpid" ] && kill "$dpid" 2>/dev/null
	[ -f "$tmp/auto.pid" ] && kill "$(cat "$tmp/auto.pid")" 2>/dev/null
	rm -rf "$tmp"
	return 0
}
trap cleanup EXIT
fail() { echo "smoke: $*" >&2; exit 1; }

# A daemon that never listens: the invoker must give up with a clear message.
if ZAZZLE_SOCK=$tmp/none ZAZZLE_DAEMON=/usr/bin/true "$zazzle" version 2>"$tmp/err"; then fail "expected failure without daemon"; fi
grep -q "daemon did not start" "$tmp/err" || fail "missing 'daemon did not start': $(cat "$tmp/err")"

export ZAZZLE_SOCK=$tmp/sock
"$zazzled" serve --socket "$ZAZZLE_SOCK" &
dpid=$!
i=0
while [ ! -S "$ZAZZLE_SOCK" ] && [ "$i" -lt 100 ]; do
	sleep 0.05
	i=$((i + 1))
done
[ -S "$ZAZZLE_SOCK" ] || fail "zazzled did not start"

mkdir -p "$tmp/work/src"
printf 'alpha one\nbeta two\nalpha three\n' > "$tmp/work/src/a.txt"
cd "$tmp/work"

out=$("$zazzle" version)
[ "$out" = "zazzle 0.1.0" ] || fail "version: '$out'"

out=$("$zazzle" find alpha)
want="src/a.txt
1: alpha one
3: alpha three
-- 2 matches in 1 files (1 searched)"
[ "$out" = "$want" ] || fail "find output: '$out'"

out=$("$zazzle" read src/a.txt:2-2)
case $out in "src/a.txt 3 lines, sha "*"[2-2]"*"2	beta two") ;; *) fail "read output: '$out'" ;; esac

if "$zazzle" bogus 2>"$tmp/err"; then fail "expected nonzero status"; fi
grep -q "error: unknown command 'bogus'" "$tmp/err" || fail "stderr: $(cat "$tmp/err")"
"$zazzle" bogus 2>/dev/null && fail "status" || [ $? -eq 2 ] || fail "exit status was not 2"

# Session ids and dedupe: the second identical read in one session is a short notice.
first=$(ZAZZLE_SESSION=smoke "$zazzle" read src/a.txt)
case $first in "[r:"*"]
src/a.txt 3 lines"*) ;; *) fail "session read: '$first'" ;; esac
second=$(ZAZZLE_SESSION=smoke "$zazzle" read src/a.txt)
case $second in "[r:"*"] already sent: zazzle read src/a.txt, unchanged. If you no longer have it: read-again r:"*) ;; *) fail "dedupe notice: '$second'" ;; esac
id=$(printf '%s' "$first" | sed -n '1s/^\[r:\([0-9a-z]*\)\].*/\1/p')
[ "$(ZAZZLE_SESSION=smoke "$zazzle" read-again "r:$id")" = "$first" ] || fail "read-again differs"
again=$(ZAZZLE_SESSION=smoke "$zazzle" read src/a.txt --disable-session-deduping)
[ "$again" = "$first" ] || fail "resend differs"

# Edit through the invoker with the new text on standard input, inside the working directory only; edits need the sha from read.
if "$zazzle" edit src/a.txt --old 'beta two' --new x 2>"$tmp/err"; then fail "edit without sha must fail"; fi
grep -q "edits need --expect SHA" "$tmp/err" || fail "missing-sha message: $(cat "$tmp/err")"
sha=$("$zazzle" read src/a.txt --source | sed -n '1s/.*sha \([0-9a-f]*\).*/\1/p')
printf 'beta from stdin' | "$zazzle" edit src/a.txt --old 'beta two' --new - --expect "$sha" >/dev/null || fail "edit with stdin"
grep -q 'beta from stdin' src/a.txt || fail "edit did not write: $(cat src/a.txt)"
if "$zazzle" edit /etc/hosts --old a --new b 2>"$tmp/err"; then fail "edit outside cwd must fail"; fi
grep -q "refusing to write outside" "$tmp/err" || fail "confinement message: $(cat "$tmp/err")"
printf 'alpha one\nbeta two\nalpha three\n' > "$tmp/work/src/a.txt"

kill "$dpid"; dpid=

# Autostart: no daemon is running; the invoker starts one that does not keep our stdout open.
export ZAZZLE_SOCK=$tmp/auto.sock ZAZZLE_DAEMON=$zazzled ZAZZLE_IDLE_SECS=30
# A pipe to cat only closes when every holder of the write end is gone, so a daemon that kept our stdout would hang here.
( "$zazzle" find beta | cat > "$tmp/auto.out" ) &
waiter=$!
i=0
while kill -0 "$waiter" 2>/dev/null && [ "$i" -lt 100 ]; do
	sleep 0.1
	i=$((i + 1))
done
if kill -0 "$waiter" 2>/dev/null; then kill "$waiter" 2>/dev/null; fail "autostart find hung: the daemon kept the caller's stdout open"; fi
out=$(cat "$tmp/auto.out")
case $out in *"2: beta two"*) ;; *) fail "autostart output: '$out'" ;; esac
status=$("$zazzle" status)
echo "$status" | sed -n 's/^zazzled [^ ]* pid \([0-9]*\) .*/\1/p' > "$tmp/auto.pid"
[ -s "$tmp/auto.pid" ] || fail "no pid in status: $status"

# The standalone tools run as their own process, found beside the invoker, with exit status and arguments passed through (no daemon involved).
mkdir "$tmp/bin"
cp "$zazzle" "$tmp/bin/zazzle"
printf '#!/bin/sh\necho "fix-network $*"\nexit 3\n' > "$tmp/bin/fix-network"
chmod +x "$tmp/bin/fix-network"
rc=0
out=$(ZAZZLE_SOCK=$tmp/none "$tmp/bin/zazzle" fix-network --verbose x 2>&1) || rc=$?
[ "$rc" = 3 ] && [ "$out" = "fix-network --verbose x" ] || fail "tool exec: rc=$rc out='$out'"
if ZAZZLE_SOCK=$tmp/none "$tmp/bin/zazzle" free-disk-space 2>"$tmp/err"; then fail "missing tool must fail"; fi
grep -q "cannot run the tool" "$tmp/err" || fail "missing tool message: $(cat "$tmp/err")"
# `zazzle proxy` and `zazzle run` exec `zazzled proxy` and `zazzled wrap` (here a stand-in from $ZAZZLE_DAEMON) with the arguments and exit status.
printf '#!/bin/sh\necho "zazzled $*"\nexit 4\n' > "$tmp/bin/fake-zazzled"
chmod +x "$tmp/bin/fake-zazzled"
rc=0
out=$(ZAZZLE_SOCK=$tmp/none ZAZZLE_DAEMON=$tmp/bin/fake-zazzled "$tmp/bin/zazzle" proxy -- srv 2>&1) || rc=$?
[ "$rc" = 4 ] && [ "$out" = "zazzled proxy -- srv" ] || fail "proxy exec: rc=$rc out='$out'"
rc=0
out=$(ZAZZLE_SOCK=$tmp/none ZAZZLE_DAEMON=$tmp/bin/fake-zazzled "$tmp/bin/zazzle" run -- make 2>&1) || rc=$?
[ "$rc" = 4 ] && [ "$out" = "zazzled wrap -- make" ] || fail "run exec: rc=$rc out='$out'"
echo "smoke ok"
