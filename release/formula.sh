#!/usr/bin/env bash
# Write the zazzle Homebrew formula for a release: [FORMULA=zazzle-alpha] formula.sh TAG SHA256 DIR [BOTTLE_DIR]
# FORMULA names the formula (default zazzle); zazzle-alpha is the alpha channel, which conflicts with zazzle.
#
# The formula installs the prebuilt universal binaries from the release tarball of TAG on GitHub: only the
# launcher, bin/zazzle, goes on PATH; every tool sits unlinked in libexec/zazzle, where `zazzle TOOL` runs it.
# The C# indexer comes compressed for both CPU types; post_install_steps expands the one that matches this Mac
# into libexec/zazzle, and links the short name z to zazzle unless something else already uses z or the user
# opted out with HOMEBREW_PREFIX/etc/zazzle/no-z.
# With BOTTLE_DIR (bottle.sh output, uploaded to the same release) it also gets a bottle block, so brew
# pours the bottle and skips its build-from-source checks, such as up-to-date Command Line Tools.
# Homebrew does not quarantine what a formula installs, so Gatekeeper lets unsigned binaries run.
set -euo pipefail

repo="david-brandt-the-icelander/zazzle"
# The tools in the tarball's and the keg's libexec/zazzle (the C# indexer is added by post-install).
tools="daemon zazz-script-runner fix-network free-disk-space git-tools git-tools-commit git-tools-commit-message-editor ai-agent-rules cyberspace-generator"
tag=${1:-}
sha=${2:-}
dir=${3:-}
bottles=${4:-}
name=${FORMULA:-zazzle}
[[ "$name" =~ ^zazzle(-alpha)?$ ]] || { echo "unknown formula '$name'; use zazzle or zazzle-alpha" >&2; exit 2; }
class=Zazzle
conflicts=""
if [ "$name" = zazzle-alpha ]; then
  class=ZazzleAlpha
  conflicts=$'\n  conflicts_with "zazzle", because: "both install the zazzle commands"'
fi
if ! [[ "$tag" =~ ^v[0-9]+(\.[0-9]+)*(-alpha\.[0-9]+)?$ ]] || ! [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || [ -z "$dir" ]; then
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
  local file="$bottles/$name-${tag#v}.all.bottle.tar.gz"
  [ -f "$file" ] || { echo "missing bottle: $file" >&2; return 1; }
  printf '\n\n  bottle do\n    root_url "https://github.com/%s/releases/download/%s"\n' "$repo" "$tag"
  printf '    sha256 cellar: :any_skip_relocation, all: "%s"\n  end\n' "$(sha256sum "$file" | cut -d' ' -f1)"
}

block=$(bottle_block)
# shellcheck disable=SC2086 # one name per word
tool_list=$(printf '"libexec/zazzle/%s", ' $tools)
cat >"$dir/$name.rb" <<RUBY
# Homebrew formula for zazzle; written by release/formula.sh on each release.
class $class < Formula
  desc "Agent tools behind one launcher: zazzle (and z), a C# indexer and macOS fixers"
  homepage "https://github.com/$repo"
  url "https://github.com/$repo/releases/download/$tag/zazzle-macos-universal.tar.gz"
  version "${tag#v}"
  sha256 "$sha"
  license "Unlicense"$block

  depends_on :macos$conflicts

  # Only the launcher is linked; the tools stay in libexec/zazzle, where \`zazzle TOOL\` finds them.
  def install
    bin.install "bin/zazzle"
    (libexec/"zazzle").install ${tool_list%, }
    (libexec/"indexer").install "libexec/indexer/csharp-indexer-arm64.gz", "libexec/indexer/csharp-indexer-x64.gz"
  end

  # Expands the C# indexer for this CPU into libexec/zazzle, then gives zazzle the short name z: an earlier z
  # of zazzle's own (either channel) is replaced, and none is made when another program has bin/z (or zoxide or
  # z.sh is installed, or /usr/local/bin/z exists) or HOMEBREW_PREFIX/etc/zazzle/no-z asks for none.
  post_install_steps do
    run "/bin/sh",
        args:           ["-c", 'set -e; a=x64; [ "\$(uname -m)" = arm64 ] && a=arm64; gunzip -c "\$1/csharp-indexer-\$a.gz" > "\$2.tmp"; chmod 755 "\$2.tmp"; mv -f "\$2.tmp" "\$2"',
                         "sh", "{{libexec}}/indexer", "{{libexec}}/zazzle/csharp-indexer"],
        writable_paths: ["zazzle/csharp-indexer", "zazzle/csharp-indexer.tmp"],
        writable_base:  :libexec
    remove "bin/z", base: :homebrew_prefix, symlink_target_contains: "/opt/zazzle"
    unless_path_exists "etc/zazzle/no-z", base: :homebrew_prefix do
      unless_path_exists "bin/z", base: :homebrew_prefix do
        unless_path_exists "bin/zoxide", base: :homebrew_prefix do
          unless_path_exists "etc/profile.d/z.sh", base: :homebrew_prefix do
            unless_path_exists "/usr/local/bin/z" do
              symlink "bin/zazzle", "bin/z", source_base: :opt_prefix, target_base: :homebrew_prefix,
                                             overwrite: true, remove_on_uninstall: true
            end
          end
        end
      end
    end
  end

  # A daemon started by an older version keeps answering until it is replaced; say how after every install.
  def caveats
    <<~EOS
      Every tool runs through the launcher: zazzle TOOL ARGS (for example: zazzle daemon restart).
      zazzle --version reports the running daemon's version. If it is older than ${tag#v}, replace it with:
        zazzle daemon restart
      z is linked to zazzle unless another z was found. To keep z for something else:
        mkdir -p #{HOMEBREW_PREFIX}/etc/zazzle && touch #{HOMEBREW_PREFIX}/etc/zazzle/no-z && brew postinstall $name
    EOS
  end

  test do
    assert_match "zazzle", shell_output("#{bin}/zazzle version")
    assert_match "daemon", shell_output("#{bin}/zazzle daemon --version")
    assert_match "zazz", shell_output("#{bin}/zazzle zazz help")
    assert_match "zazz-script-runner", shell_output("#{libexec}/zazzle/zazz-script-runner version")
    assert_match "fix-network", shell_output("#{bin}/zazzle fix-network --version")
    assert_match "free-disk-space", shell_output("#{bin}/zazzle free-disk-space --version")
    assert_match "git-tools", shell_output("#{bin}/zazzle git-tools --version")
    assert_match "ai-agent-rules", shell_output("#{bin}/zazzle ai-agent-rules --version")
    assert_match "csharp-indexer", shell_output("#{bin}/zazzle csharp-indexer --version")
    assert_match "cyberspace-generator", shell_output("#{bin}/zazzle cyberspace-generator --version")
  end
end
RUBY
echo "wrote $dir/$name.rb for $tag"
