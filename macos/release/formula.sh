#!/usr/bin/env bash
# Write the zazzle Homebrew formula for a release: formula.sh TAG SHA256 DIR [BOTTLE_DIR]
#
# The formula installs the prebuilt universal binaries from the release tarball of TAG on GitHub, and the
# compressed C# indexer for both CPU types; post_install expands the one that matches this Mac.
# With BOTTLE_DIR (bottle.sh output, uploaded to the same release) it also gets a bottle block, so brew
# pours the bottle and skips its build-from-source checks, such as up-to-date Command Line Tools.
# Homebrew does not quarantine what a formula installs, so Gatekeeper lets unsigned binaries run.
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

# Print the bottle block, or nothing without bottles; fails if the bottle is missing.
bottle_block() {
  [ -n "$bottles" ] || return 0
  local file="$bottles/zazzle-${tag#v}.all.bottle.tar.gz"
  [ -f "$file" ] || { echo "missing bottle: $file" >&2; return 1; }
  printf '\n\n  bottle do\n    root_url "https://github.com/%s/releases/download/%s"\n' "$repo" "$tag"
  printf '    sha256 cellar: :any_skip_relocation, all: "%s"\n  end\n' "$(sha256sum "$file" | cut -d' ' -f1)"
}

block=$(bottle_block)
cat >"$dir/zazzle.rb" <<RUBY
# Homebrew formula for zazzle; written by macos/release/formula.sh on each release.
class Zazzle < Formula
  desc "Agent file tools: zazzle, its zazzled daemon, a C# code indexer, free-disk-space and fix-network"
  homepage "https://github.com/$repo"
  url "https://github.com/$repo/releases/download/$tag/zazzle-macos-universal.tar.gz"
  version "${tag#v}"
  sha256 "$sha"
  license "Unlicense"$block

  depends_on :macos

  def install
    bin.install "zazzle", "zazzled", "afs", "fix-network", "free-disk-space"
    (libexec/"indexer").install "zazzle-csharp-indexer-arm64.gz", "zazzle-csharp-indexer-x64.gz"
  end

  # Expand the C# indexer for this CPU (compressed to keep the download and the bottle small).
  def post_install
    arch = Hardware::CPU.arm? ? "arm64" : "x64"
    system "sh", "-c", 'gunzip -c "\$1" > "\$2" && chmod 755 "\$2"', "sh",
           libexec/"indexer/zazzle-csharp-indexer-#{arch}.gz", bin/"zazzle-csharp-indexer"
  end

  test do
    assert_match "zazzle", shell_output("#{bin}/zazzle version")
    assert_match "zazzled", shell_output("#{bin}/zazzled --version")
    assert_match "fix-network", shell_output("#{bin}/fix-network --version")
    assert_match "free-disk-space", shell_output("#{bin}/free-disk-space --version")
    assert_match "agent-fs", shell_output("#{bin}/afs --version")
    assert_match "zazzle-csharp-indexer", shell_output("#{bin}/zazzle-csharp-indexer --version")
  end
end
RUBY
echo "wrote $dir/zazzle.rb for $tag"
