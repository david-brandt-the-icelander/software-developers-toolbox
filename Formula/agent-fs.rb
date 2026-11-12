# Homebrew formula for agent-fs; written by macos/release/formula.sh on each release.
class AgentFs < Formula
  desc "Read, search and outline files with cached JSON replies for agents (afs)"
  homepage "https://github.com/david-brandt-the-icelander/software-developers-toolbox/tree/main/macos/rust/agent-fs"
  url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.0/toolbox-macos-universal.tar.gz"
  version "1.1.0"
  sha256 "3c05c254ed3371fa3011469447238ec81a58c5da5cbc3a36f4ef7f98607d48d6"
  license "Unlicense"

  depends_on :macos

  def install
    bin.install "afs"
  end

  test do
    assert_match "agent-fs", shell_output("#{bin}/afs --version")
  end
end
