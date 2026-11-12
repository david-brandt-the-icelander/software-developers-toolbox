# Homebrew formula for fix-network; written by macos/release/formula.sh on each release.
class FixNetwork < Formula
  desc "Diagnose and repair macOS Wi-Fi, DNS, routing, proxy and VPN problems"
  homepage "https://github.com/david-brandt-the-icelander/software-developers-toolbox/tree/main/macos/rust/fix-network"
  url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.3/toolbox-macos-universal.tar.gz"
  version "1.1.3"
  sha256 "bf726e108cb31137a8f045715e57efd497925676d5eceae1d4766e38e366227c"
  license "Unlicense"

  bottle do
    root_url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.3"
    sha256 cellar: :any_skip_relocation, all: "18f2bd022974abfeff6d80c04fac15633fbfe995af4068ddca88ca03232e993b"
  end

  depends_on :macos

  def install
    bin.install "fix-network"
  end

  test do
    assert_match "fix-network", shell_output("#{bin}/fix-network --version")
  end
end
