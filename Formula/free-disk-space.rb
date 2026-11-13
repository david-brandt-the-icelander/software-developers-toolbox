# Homebrew formula for free-disk-space; written by macos/release/formula.sh on each release.
class FreeDiskSpace < Formula
  desc "Reclaim disk space on APFS: compress, dedupe and clean caches safely"
  homepage "https://github.com/david-brandt-the-icelander/software-developers-toolbox/tree/main/macos/rust/free-disk-space"
  url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.3/toolbox-macos-universal.tar.gz"
  version "1.1.3"
  sha256 "bf726e108cb31137a8f045715e57efd497925676d5eceae1d4766e38e366227c"
  license "Unlicense"

  bottle do
    root_url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.3"
    sha256 cellar: :any_skip_relocation, all: "b21c2bcbd8e2d74a68021d31633d6044d0e8f9fa2ed1a4635d317db079c02c34"
  end

  depends_on :macos

  def install
    bin.install "free-disk-space"
  end

  test do
    assert_match "free-disk-space", shell_output("#{bin}/free-disk-space --version")
  end
end
