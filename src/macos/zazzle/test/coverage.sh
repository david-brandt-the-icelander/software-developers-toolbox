#!/bin/sh
# Fails unless gcov's "branches taken at least once" for zazzle_core.h meets the given percentage.
set -eu
report=$1
min=$2
pct=$(awk '/^File .*zazzle_core.h/ {found=1} found && /Taken at least once/ {gsub(/[^0-9.]/, "", $4); print $4; exit}' "$report" | sed 's/%//')
[ -n "$pct" ] || { echo "no branch data for zazzle_core.h" >&2; exit 1; }
echo "zazzle_core.h branches taken: ${pct}%"
awk -v p="$pct" -v m="$min" 'BEGIN { exit !(p + 0 >= m + 0) }' || { echo "branch coverage below ${min}%" >&2; exit 1; }
