#!/bin/sh
# End-to-end check of the invoker against the real daemon: output, status, errors and autostart.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$here/..
afo=$root/build/afo
afod=${AFOD:-$root/../../rust/target/release/afod}
# Absolute, because the checks below cd into a work directory and autostart runs it from there.
case $afod in /*) ;; *) afod=$(pwd)/$afod ;; esac
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
if AFO_SOCK=$tmp/none AFO_DAEMON=/usr/bin/true "$afo" version 2>"$tmp/err"; then fail "expected failure without daemon"; fi
grep -q "daemon did not start" "$tmp/err" || fail "missing 'daemon did not start': $(cat "$tmp/err")"

export AFO_SOCK=$tmp/sock
"$afod" serve --socket "$AFO_SOCK" &
dpid=$!
i=0
while [ ! -S "$AFO_SOCK" ] && [ "$i" -lt 100 ]; do
	sleep 0.05
	i=$((i + 1))
done
[ -S "$AFO_SOCK" ] || fail "afod did not start"

mkdir -p "$tmp/work/src"
printf 'alpha one\nbeta two\nalpha three\n' > "$tmp/work/src/a.txt"
cd "$tmp/work"

out=$("$afo" version)
[ "$out" = "afo 0.1.0" ] || fail "version: '$out'"

out=$("$afo" find alpha)
want="src/a.txt
1: alpha one
3: alpha three
-- 2 matches in 1 files (1 searched)"
[ "$out" = "$want" ] || fail "find output: '$out'"

out=$("$afo" read src/a.txt:2-2)
case $out in "src/a.txt 3 lines, sha "*"[2-2]"*"2	beta two") ;; *) fail "read output: '$out'" ;; esac

if "$afo" bogus 2>"$tmp/err"; then fail "expected nonzero status"; fi
grep -q "error: unknown command 'bogus'" "$tmp/err" || fail "stderr: $(cat "$tmp/err")"
"$afo" bogus 2>/dev/null && fail "status" || [ $? -eq 2 ] || fail "exit status was not 2"

# Session ids and dedupe: the second identical read in one session is a short notice.
first=$(AFO_SESSION=smoke "$afo" read src/a.txt)
case $first in "[r:"*"]
src/a.txt 3 lines"*) ;; *) fail "session read: '$first'" ;; esac
second=$(AFO_SESSION=smoke "$afo" read src/a.txt)
case $second in "[r:"*"] already sent: afo read src/a.txt, unchanged. Re-run with --disable-session-deduping to resend.") ;; *) fail "dedupe notice: '$second'" ;; esac
again=$(AFO_SESSION=smoke "$afo" read src/a.txt --disable-session-deduping)
[ "$again" = "$first" ] || fail "resend differs"

# Edit through the invoker with the new text on standard input, inside the working directory only.
printf 'beta from stdin' | "$afo" edit src/a.txt --old 'beta two' --new - >/dev/null || fail "edit with stdin"
grep -q 'beta from stdin' src/a.txt || fail "edit did not write: $(cat src/a.txt)"
if "$afo" edit /etc/hosts --old a --new b 2>"$tmp/err"; then fail "edit outside cwd must fail"; fi
grep -q "refusing to write outside" "$tmp/err" || fail "confinement message: $(cat "$tmp/err")"
printf 'alpha one\nbeta two\nalpha three\n' > "$tmp/work/src/a.txt"

kill "$dpid"; dpid=

# Autostart: no daemon is running; the invoker starts one that does not keep our stdout open.
export AFO_SOCK=$tmp/auto.sock AFO_DAEMON=$afod AFO_IDLE_SECS=30
# A pipe to cat only closes when every holder of the write end is gone, so a daemon that kept our stdout would hang here.
( "$afo" find beta | cat > "$tmp/auto.out" ) &
waiter=$!
i=0
while kill -0 "$waiter" 2>/dev/null && [ "$i" -lt 100 ]; do
	sleep 0.1
	i=$((i + 1))
done
if kill -0 "$waiter" 2>/dev/null; then kill "$waiter" 2>/dev/null; fail "autostart find hung: the daemon kept the caller's stdout open"; fi
out=$(cat "$tmp/auto.out")
case $out in *"2: beta two"*) ;; *) fail "autostart output: '$out'" ;; esac
status=$("$afo" status)
echo "$status" | sed -n 's/^afod [^ ]* pid \([0-9]*\) .*/\1/p' > "$tmp/auto.pid"
[ -s "$tmp/auto.pid" ] || fail "no pid in status: $status"
echo "smoke ok"
