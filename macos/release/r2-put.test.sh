#!/usr/bin/env bash
# Tests r2-put.sh against a stand-in npx that fails a chosen number of times.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

cat >"$tmp/npx" <<'STUB'
#!/usr/bin/env bash
echo "$*" >>"$STUB_DIR/calls"
n=$(wc -l <"$STUB_DIR/calls")
[ "$n" -gt "$STUB_FAILS" ]
STUB
chmod +x "$tmp/npx"

# Runs r2-put.sh with the stand-in failing FAILS times; prints the number of calls it made.
run() {
  rm -f "$tmp/calls"
  PATH="$tmp:$PATH" STUB_DIR=$tmp STUB_FAILS=$1 R2_RETRY_SECONDS=0 bash "$here/r2-put.sh" zazzle/v1.2.3/a.tar.gz a.tar.gz application/gzip 2>/dev/null
  status=$?
  wc -l <"$tmp/calls" | tr -d ' '
  return $status
}

[ "$(run 0)" = 1 ] || fail "first try"
[ "$(run 3)" = 4 ] || fail "retried until success"
grep -q -- '--yes wrangler@4 r2 object put icelander-downloads/zazzle/v1.2.3/a.tar.gz --file a.tar.gz --content-type application/gzip --remote' "$tmp/calls" || fail "arguments"
if calls=$(run 9); then fail "succeeded although every attempt failed"; fi
[ "$calls" = 5 ] || fail "gave up after $calls attempts, not 5"
echo "r2-put ok"
