#!/usr/bin/env bash
# Write one Homebrew formula per tool for a release: formula.sh TAG SHA256 DIR (normally DIR=Formula).
#
# Each formula installs its prebuilt universal binary from the release tarball of TAG on GitHub.
# Homebrew does not quarantine what a formula installs, so Gatekeeper lets unsigned binaries run.
# Every formula takes the release tag as its version, so each release is an upgrade for all of them.
set -euo pipefail

repo="david-brandt-the-icelander/software-developers-toolbox"
tag=${1:-}
sha=${2:-}
dir=${3:-}
if ! [[ "$tag" =~ ^v[0-9]+(\.[0-9]+)*$ ]] || ! [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || [ -z "$dir" ]; then
  echo "usage: $0 vX.Y.Z SHA256 DIR   (tag like v2.0.0, lowercase 64-hex sha256)" >&2
  exit 2
fi
mkdir -p "$dir"

# Write one formula: name, Ruby class, binary, version-output name, description, source folder.
formula() {
  cat >"$dir/$1.rb" <<EOF
# Homebrew formula for $1; written by macos/release/formula.sh on each release.
class $2 < Formula
  desc "$5"
  homepage "https://github.com/$repo/tree/main/macos/rust/$6"
  url "https://github.com/$repo/releases/download/$tag/toolbox-macos-universal.tar.gz"
  version "${tag#v}"
  sha256 "$sha"
  license "Unlicense"

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
