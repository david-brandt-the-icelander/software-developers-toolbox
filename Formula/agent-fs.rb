# Homebrew formula for agent-fs; written by macos/release/formula.sh on each release.
class AgentFs < Formula
  desc "Read, search and outline files with cached JSON replies for agents (afs)"
  homepage "https://github.com/david-brandt-the-icelander/software-developers-toolbox/tree/main/macos/rust/agent-fs"
  url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.3/toolbox-macos-universal.tar.gz"
  version "1.1.3"
  sha256 "bf726e108cb31137a8f045715e57efd497925676d5eceae1d4766e38e366227c"
  license "Unlicense"

  bottle do
    root_url "https://github.com/david-brandt-the-icelander/software-developers-toolbox/releases/download/v1.1.3"
    sha256 cellar: :any_skip_relocation, all: "0e857d93715f33e8a2a4804b50fdd0319a229d9e513c155c918744e56c9f39f0"
  end

  depends_on :macos

  def install
    bin.install "afs"
  end

  test do
    assert_match "agent-fs", shell_output("#{bin}/afs --version")
  end
end
