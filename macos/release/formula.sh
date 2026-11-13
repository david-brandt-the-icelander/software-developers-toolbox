#!/usr/bin/env bash
# Write the zazzle Homebrew formula for a release: formula.sh TAG SHA256 DIR [BOTTLE_DIR]
#
# The formula installs the prebuilt universal binaries from the release tarball of TAG on GitHub, and the
# compressed C# indexer for both CPU types; post_install_steps expands the one that matches this Mac over a
# placeholder bin/zazzle-csharp-indexer, which brew links before post-install runs.
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
    bin.install "zazzle", "zazz", "zazzled", "zazz-worker", "afs", "fix-network", "free-disk-space"
    (libexec/"indexer").install "zazzle-csharp-indexer-arm64.gz", "zazzle-csharp-indexer-x64.gz"
    (bin/"zazzle-csharp-indexer").write <<~SH
      #!/bin/sh
      echo "zazzle-csharp-indexer is not set up yet; run: brew postinstall zazzle" >&2
      exit 1
    SH
    chmod 0755, bin/"zazzle-csharp-indexer"
  end

  # Expand the C# indexer for this CPU over the placeholder, which brew linked before post-install ran.
  post_install_steps do
    run "/bin/sh",
        args:           ["-c", 'set -e; a=x64; [ "\$(uname -m)" = arm64 ] && a=arm64; gunzip -c "\$1/zazzle-csharp-indexer-\$a.gz" > "\$2.tmp"; chmod 755 "\$2.tmp"; mv -f "\$2.tmp" "\$2"',
                         "sh", "{{libexec}}/indexer", "{{bin}}/zazzle-csharp-indexer"],
        writable_paths: ["zazzle-csharp-indexer", "zazzle-csharp-indexer.tmp"],
        writable_base:  :bin
  end

  test do
    assert_match "zazzle", shell_output("#{bin}/zazzle version")
    assert_match "zazzled", shell_output("#{bin}/zazzled --version")
    assert_match "zazz", shell_output("#{bin}/zazz help")
    assert_match "zazz-worker", shell_output("#{bin}/zazz-worker version")
    assert_match "fix-network", shell_output("#{bin}/fix-network --version")
    assert_match "free-disk-space", shell_output("#{bin}/free-disk-space --version")
    assert_match "agent-fs", shell_output("#{bin}/afs --version")
    assert_match "zazzle-csharp-indexer", shell_output("#{bin}/zazzle-csharp-indexer --version")
  end
end
RUBY
echo "wrote $dir/zazzle.rb for $tag"
