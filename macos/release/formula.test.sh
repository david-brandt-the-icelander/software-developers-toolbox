#!/usr/bin/env bash
# Test formula.sh: valid Ruby, correct DSL values under a stand-in Formula class, and consistency with the crates and release.yml.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0
sha=$(printf '%064d' 7 | tr 0 a)

# Record a failed expectation.
fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

# 1. Bad input: usage error and no files.
for args in "" "2.0.0 $sha $tmp/x" "v2.0.0 ABC $tmp/x" "v2.0.0 $sha"; do
  # shellcheck disable=SC2086
  if bash "$here/formula.sh" $args >/dev/null 2>&1; then fail "accepted bad input: '$args'"; fi
done
[ ! -e "$tmp/x" ] || fail "bad input wrote files"

# 2. Generate the formulas for a release.
out=$(bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/Formula")
case "$out" in *"for v2.1.0"*) ;; *) fail "summary line: $out" ;; esac
[ "$(find "$tmp/Formula" -name '*.rb' | wc -l)" -eq 3 ] || fail "wrote other than 3 formulas"

# 3. Load each formula under a stand-in Homebrew Formula class and check every DSL value, install and test.
cat >"$tmp/check.rb" <<'RUBY'
# Stand-in for Homebrew's Formula: records DSL values, installs into a fake bin, runs the test block.
class Formula
  class << self
    attr_reader :values, :test_block
    def method_missing(name, *args)
      (@values ||= {})[name] = args.length == 1 ? args.first : args
    end
    def respond_to_missing?(*) = true
    def test(&block) = @test_block = block
  end
  # Records installed file names.
  class Bin
    attr_reader :installed
    def install(*files) = (@installed ||= []).concat(files)
    def to_s = "/opt/homebrew/bin"
  end
  attr_reader :bin, :commands
  def initialize = (@bin = Bin.new)
  def shell_output(cmd) = ((@commands ||= []) << cmd) && "#{ENV.fetch("VERSION_NAME")} 2.1.0\n"
  def assert_match(pattern, text) = (text.include?(pattern) or raise "#{pattern.inspect} not in #{text.inspect}")
end

file, klass, binary, version_name, sha = ARGV
load file
f = Object.const_get(klass)
v = f.values
repo = "https://github.com/david-brandt-the-icelander/software-developers-toolbox"
raise "url #{v[:url]}" unless v[:url] == "#{repo}/releases/download/v2.1.0/toolbox-macos-universal.tar.gz"
raise "version #{v[:version]}" unless v[:version] == "2.1.0"
raise "sha256" unless v[:sha256] == sha
raise "license" unless v[:license] == "Unlicense"
raise "depends_on" unless v[:depends_on] == :macos
raise "homepage #{v[:homepage]}" unless v[:homepage].start_with?("#{repo}/tree/main/macos/rust/")
raise "desc" unless v[:desc].is_a?(String) && v[:desc].length.between?(20, 80) && !v[:desc].end_with?(".")
inst = f.new
inst.install
raise "installed #{inst.bin.installed}" unless inst.bin.installed == [binary]
ENV["VERSION_NAME"] = version_name
inst.instance_eval(&f.test_block)
raise "test ran #{inst.commands}" unless inst.commands == ["/opt/homebrew/bin/#{binary} --version"]
puts "ok #{klass}"
RUBY

# Check one formula: file name, class, binary, the crate folder whose Cargo.toml name --version prints.
check() {
  local file="$tmp/Formula/$1.rb" crate_name
  ruby -c "$file" >/dev/null || fail "$1.rb is not valid Ruby"
  crate_name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$root/macos/rust/$4/Cargo.toml" | head -1)
  VERSION_NAME=x ruby "$tmp/check.rb" "$file" "$2" "$3" "$crate_name" "$sha" >/dev/null || fail "$1.rb failed the DSL check"
  grep -q "for bin in .*\b$3\b" "$root/.github/workflows/release.yml" || fail "release.yml does not build $3"
  grep -q "tree/main/macos/rust/$4\"" "$file" || fail "$1.rb homepage is not macos/rust/$4"
}
check fix-network FixNetwork fix-network fix-network
check free-disk-space FreeDiskSpace free-disk-space free-disk-space
check agent-fs AgentFs afs agent-fs

# 4. A wrong --version name must be caught by the formula's own test block.
if VERSION_NAME=x ruby "$tmp/check.rb" "$tmp/Formula/agent-fs.rb" AgentFs afs afs "$sha" >/dev/null 2>&1; then
  fail "the agent-fs test block accepted 'afs 2.1.0' though the binary prints 'agent-fs'"
fi

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "formula.sh: all checks passed"
