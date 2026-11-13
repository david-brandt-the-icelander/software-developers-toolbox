#!/usr/bin/env bash
# Prints this release run's tag, version and whether to publish, as GitHub step outputs: a pushed v* tag as is, else the next version after the latest v* tag for a bump, else a development build.
# usage: version.sh REF_TYPE REF_NAME BUMP   (BUMP: none, patch, minor or major)
set -euo pipefail

ref_type=$1 ref_name=$2 bump=$3

# Prints the version after the latest vX.Y.Z tag for a bump; v0.0.0 counts as the latest when there is none.
next() {
  local latest major minor patch
  latest=$(git tag -l 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname | head -1)
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
elif [ "$bump" != none ]; then
  tag=$(next "$bump") publish=true
else
  tag="" publish=false
fi
version=${tag#v}
printf 'tag=%s\nversion=%s\npublish=%s\n' "$tag" "${version:-0.0.0-dev}" "$publish"
