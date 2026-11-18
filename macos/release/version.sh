#!/usr/bin/env bash
# Prints this release run's tag, version and whether to publish, as GitHub step outputs: a pushed v* tag as is, else an
# exact VERSION when given, else the next version after the latest v* tag for a bump, else a development build.
# With CHANNEL it also prints the alpha tag (vX.Y.Z-alpha.N, N one past this version's alpha tags) and whether the
# stable release runs: CHANNEL both builds the alpha and the release, alpha only the alpha (then the exact VERSION may
# already be released).
# usage: version.sh REF_TYPE REF_NAME BUMP [VERSION] [CHANNEL]   (BUMP: none, patch, minor or major; VERSION: X.Y.Z or vX.Y.Z; CHANNEL: both or alpha)
set -euo pipefail

ref_type=$1 ref_name=$2 bump=$3 exact=${4:-} channel=${5:-}
case $channel in "" | both | alpha) ;; *) echo "unknown channel '$channel'; use both or alpha" >&2; exit 1 ;; esac

# Prints the version after the latest vX.Y.Z tag for a bump; v0.0.0 counts as the latest when there is none.
next() {
  local latest major minor patch
  latest=$(git tag -l 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -1 || true)
  IFS=. read -r major minor patch <<<"${latest:-v0.0.0}"
  major=${major#v}
  case $1 in
    major) echo "v$((major + 1)).0.0" ;;
    minor) echo "v$major.$((minor + 1)).0" ;;
    patch) echo "v$major.$minor.$((patch + 1))" ;;
    *) echo "unknown bump '$1'; use none, patch, minor or major" >&2; return 1 ;;
  esac
}

if [ "$ref_type" = tag ]; then
  [[ $ref_name =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "tag '$ref_name' is not vX.Y.Z" >&2; exit 1; }
  tag=$ref_name publish=true
elif [ -n "$exact" ]; then
  tag=v${exact#v} publish=true
  [[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version '$exact' is not X.Y.Z" >&2; exit 1; }
  [ "$channel" = alpha ] || [ -z "$(git tag -l "$tag")" ] || { echo "$tag already exists" >&2; exit 1; }
elif [ "$bump" != none ]; then
  tag=$(next "$bump") publish=true
else
  tag="" publish=false
fi
version=${tag#v}
if [ "$channel" = alpha ]; then
  publish=false
fi
printf 'tag=%s\nversion=%s\npublish=%s\n' "$tag" "${version:-0.0.0-dev}" "$publish"
if [ -n "$channel" ]; then
  alpha_tag=""
  if [ -n "$tag" ]; then
    n=$(git tag -l "$tag-alpha.*" | grep -cE "^$tag-alpha\.[0-9]+$" || true)
    alpha_tag="$tag-alpha.$((n + 1))"
  fi
  stable=true
  [ "$channel" = alpha ] && stable=false
  printf 'alpha_tag=%s\nalpha_version=%s\nstable=%s\n' "$alpha_tag" "${alpha_tag#v}" "$stable"
fi
