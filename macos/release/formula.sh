#!/usr/bin/env bash
# Write one Homebrew formula per tool for a release: formula.sh TAG SHA256 DIR [BOTTLE_DIR]
#
# Each formula installs its prebuilt universal binary from the release tarball of TAG on GitHub.
# With BOTTLE_DIR (bottle.sh output, uploaded to the same release) each also gets a bottle block, so
# brew pours the bottle and skips its build-from-source checks, such as up-to-date Command Line Tools.
# Homebrew does not quarantine what a formula installs, so Gatekeeper lets unsigned binaries run.
# Every formula takes the release tag as its version, so each release is an upgrade for all of them.
set -euo pipefail

repo="david-brandt-the-icelander/software-developers-toolbox"
tag=${1:-}
sha=${2:-}
dir=${3:-}
bottles=${4:-}
if ! [[ "$tag" =~ ^v[0-9]+(\.[0-9]+)*$ ]] || ! [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || [ -z "$dir" ]; then
  echo "usage: $0 vX.Y.Z SHA256 DIR [BOTTLE_DIR]   (tag like v2.0.0, lowercase 64-hex sha256)" >&2
  exit 2
fi
if [ -n "$bottles" ] && [ ! -d "$bottles" ]; then
  echo "no bottle directory: $bottles" >&2
  exit 2
fi
mkdir -p "$dir"

# Print the bottle block for a formula, or nothing without bottles; fails if its bottle is missing.
bottle_block() {
  [ -n "$bottles" ] || return 0
  local file="$bottles/$1-${tag#v}.all.bottle.tar.gz"
  [ -f "$file" ] || { echo "missing bottle: $file" >&2; return 1; }
  printf '\n\n  bottle do\n    root_url "https://github.com/%s/releases/download/%s"\n' "$repo" "$tag"
  printf '    sha256 cellar: :any_skip_relocation, all: "%s"\n  end\n' "$(sha256sum "$file" | cut -d' ' -f1)"
}

# Write one formula: name, Ruby class, binary, version-output name, description, source folder.
formula() {
  local block
  block=$(bottle_block "$1")
  cat >"$dir/$1.rb" <<EOF
# Homebrew formula for $1; written by macos/release/formula.sh on each release.
class $2 < Formula
  desc "$5"
  homepage "https://github.com/$repo/tree/main/macos/rust/$6"
  url "https://github.com/$repo/releases/download/$tag/toolbox-macos-universal.tar.gz"
  version "${tag#v}"
  sha256 "$sha"
  license "Unlicense"$block

  depends_on :macos

  def install
    bin.install "$3"
  end

  test do
    assert_match "$4", shell_output("#{bin}/$3 --version")
  end
end
EOF
}

formula fix-network FixNetwork fix-network fix-network \
  "Diagnose and repair macOS Wi-Fi, DNS, routing, proxy and VPN problems" fix-network
formula free-disk-space FreeDiskSpace free-disk-space free-disk-space \
  "Reclaim disk space on APFS: compress, dedupe and clean caches safely" free-disk-space
formula agent-fs AgentFs afs agent-fs \
  "Read, search and outline files with cached JSON replies for agents (afs)" agent-fs
echo "wrote $dir/fix-network.rb, $dir/free-disk-space.rb, $dir/agent-fs.rb for $tag"
