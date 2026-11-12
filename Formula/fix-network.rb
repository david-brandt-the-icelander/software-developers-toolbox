# Homebrew formula for fix-network; written by macos/release/formula.sh on each release.
class FixNetwork < Formula
  desc "Diagnose and repair macOS Wi-Fi, DNS, routing, proxy and VPN problems"
  homepage "https://github.com/david-brandt-the-icelander/software-developers-toolbox/tree/main/macos/rust/fix-network"
  url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.0/toolbox-macos-universal.tar.gz"
  version "1.1.0"
  sha256 "3c05c254ed3371fa3011469447238ec81a58c5da5cbc3a36f4ef7f98607d48d6"
  license "Unlicense"

  depends_on :macos

  def install
    bin.install "fix-network"
  end

  test do
    assert_match "fix-network", shell_output("#{bin}/fix-network --version")
  end
end
