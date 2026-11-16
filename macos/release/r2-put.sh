#!/usr/bin/env bash
# Uploads one file to icelander.net's R2 bucket, retrying because Cloudflare's API sometimes times out (HTTP 524).
# usage: r2-put.sh KEY FILE CONTENT_TYPE   (KEY is under icelander-downloads/; R2_RETRY_SECONDS overrides the first wait)
set -euo pipefail

key=$1 file=$2 type=$3 wait=${R2_RETRY_SECONDS:-15}
for attempt in 1 2 3 4 5; do
  npx --yes wrangler@4 r2 object put "icelander-downloads/$key" --file "$file" --content-type "$type" --remote && exit 0
  [ "$attempt" -lt 5 ] || break
  echo "upload of $key failed (attempt $attempt); retrying in ${wait}s" >&2
  sleep "$wait"
  wait=$((wait * 2))
done
echo "upload of $key failed after 5 attempts" >&2
exit 1
