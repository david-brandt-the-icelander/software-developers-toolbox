#!/bin/sh
# Fails unless branch coverage of a crate (default zazzled, test code excluded) meets the given percentage, default 95.
set -eu
min=${1:-95}
crate=${2:-zazzled}
pct=$(cargo +nightly llvm-cov --branch -p "$crate" --json --summary-only | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"][0]["totals"]["branches"]["percent"])')
echo "$crate branch coverage: ${pct}%"
python3 -c "import sys; sys.exit(0 if float('$pct') >= float('$min') else 1)"
