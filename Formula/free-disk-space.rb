# Homebrew formula for free-disk-space; written by macos/release/formula.sh on each release.
class FreeDiskSpace < Formula
  desc "Reclaim disk space on APFS: compress, dedupe and clean caches safely"
  homepage "https://github.com/david-brandt-the-icelander/software-developers-toolbox/tree/main/macos/rust/free-disk-space"
  url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.0/toolbox-macos-universal.tar.gz"
  version "1.1.0"
  sha256 "3c05c254ed3371fa3011469447238ec81a58c5da5cbc3a36f4ef7f98607d48d6"
  license "Unlicense"

  depends_on :macos

  def install
    bin.install "free-disk-space"
  end

  test do
    assert_match "free-disk-space", shell_output("#{bin}/free-disk-space --version")
  end
end
