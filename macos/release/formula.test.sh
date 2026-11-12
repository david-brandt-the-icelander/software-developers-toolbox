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
    def bottle(&block) = (@bottle = BottleSpec.new).instance_eval(&block)
    def bottle_spec = @bottle
  end
  # Records a bottle block's root_url and sha256 entries.
  class BottleSpec
    attr_reader :url, :checksums
    def root_url(url) = @url = url
    def sha256(**tags) = (@checksums ||= []) << tags
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
bottles = ENV.fetch("BOTTLES", "")
spec = f.bottle_spec
if bottles.empty?
  raise "unexpected bottle block" if spec
else
  raise "no bottle block" unless spec
  raise "root_url #{spec.url}" unless spec.url == "#{repo}/releases/download/v2.1.0"
  raise "bottle entries #{spec.checksums}" unless spec.checksums.length == 1
  entry = spec.checksums.first
  raise "cellar #{entry}" unless entry[:cellar] == :any_skip_relocation && entry.keys.sort == [:all, :cellar]
  # Homebrew downloads root_url/NAME-VERSION.TAG.bottle.tar.gz for a plain root_url (Bottle::Filename#url_encode).
  name = File.basename(file, ".rb")
  path = File.join(bottles, "#{name}-#{v[:version]}.all.bottle.tar.gz")
  raise "Homebrew would fetch #{File.basename(path)}, which bottle.sh did not write" unless File.exist?(path)
  require "digest"
  raise "bottle sha256 mismatch" unless entry[:all] == Digest::SHA256.file(path).hexdigest
end
puts "ok #{klass}"
RUBY

# Check one formula: file name, class, binary, the crate folder whose Cargo.toml name --version prints.
check() {
  local file="$tmp/Formula/$1.rb" crate_name
  ruby -c "$file" >/dev/null || fail "$1.rb is not valid Ruby"
  crate_name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$root/macos/rust/$4/Cargo.toml" | head -1)
  BOTTLES="${bottles:-}" ruby "$tmp/check.rb" "$file" "$2" "$3" "$crate_name" "$sha" >/dev/null || fail "$1.rb failed the DSL check"
  grep -q "for bin in .*\b$3\b" "$root/.github/workflows/release.yml" || fail "release.yml does not build $3"
  grep -q "tree/main/macos/rust/$4\"" "$file" || fail "$1.rb homepage is not macos/rust/$4"
}
check fix-network FixNetwork fix-network fix-network
check free-disk-space FreeDiskSpace free-disk-space free-disk-space
check agent-fs AgentFs afs agent-fs

# 4. Bottles: bad input, then one reproducible bottle per tool in exactly the layout Homebrew pours.
for args in "" "2.1.0 $tmp/bins $tmp/b" "v2.1.0 $tmp/none $tmp/b"; do
  # shellcheck disable=SC2086
  if bash "$here/bottle.sh" $args >/dev/null 2>&1; then fail "bottle.sh accepted bad input: '$args'"; fi
done
mkdir -p "$tmp/bins"
printf '#!/bin/sh\n' >"$tmp/bins/fix-network"
if bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/partial" >/dev/null 2>&1; then fail "bottle.sh accepted a missing binary"; fi
for b in afs free-disk-space; do printf '#!/bin/sh\n' >"$tmp/bins/$b"; done
chmod 0644 "$tmp/bins/afs"
bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/bottles" >"$tmp/bottle.out"
bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/again" >/dev/null
for pair in fix-network:fix-network free-disk-space:free-disk-space agent-fs:afs; do
  name=${pair%%:*} binary=${pair##*:} file="$tmp/bottles/${pair%%:*}-2.1.0.all.bottle.tar.gz"
  [ -f "$file" ] || { fail "no bottle for $name"; continue; }
  grep -q "^$(sha256sum "$file" | cut -d' ' -f1)  $name-2.1.0.all.bottle.tar.gz$" "$tmp/bottle.out" || fail "$name: printed sha256"
  cmp -s "$file" "$tmp/again/$(basename "$file")" || fail "$name: bottle is not reproducible"
  listing=$(tar tzf "$file" | tr '\n' ' ')
  [ "$listing" = "$name/ $name/2.1.0/ $name/2.1.0/bin/ $name/2.1.0/bin/$binary " ] || fail "$name: contents $listing"
  tar tvzf "$file" | grep -q "^-rwxr-xr-x 0/0 .* $name/2.1.0/bin/$binary$" || fail "$name: binary not 0755 root-owned"
  # Homebrew's own check (Bottle#extract_snapshot): exactly one NAME directory holding exactly one VERSION directory.
  rm -rf "$tmp/x" && mkdir "$tmp/x" && tar xzf "$file" -C "$tmp/x"
  [ "$(ls -A "$tmp/x")" = "$name" ] && [ "$(ls -A "$tmp/x/$name")" = "2.1.0" ] || fail "$name: not NAME/VERSION only"
done

# 5. Formulas with bottles: each gets a bottle block pointing at its file, checked under the stand-in Formula.
bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/Formula" "$tmp/bottles" >/dev/null
bottles="$tmp/bottles"
check fix-network FixNetwork fix-network fix-network
check free-disk-space FreeDiskSpace free-disk-space free-disk-space
check agent-fs AgentFs afs agent-fs
bottles=""
mv "$tmp/bottles/agent-fs-2.1.0.all.bottle.tar.gz" "$tmp/agent-fs.bottle"
if bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/F2" "$tmp/bottles" >/dev/null 2>&1; then fail "formula.sh accepted a missing bottle"; fi
if bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/F3" "$tmp/no-such-dir" >/dev/null 2>&1; then fail "formula.sh accepted a missing bottle dir"; fi

# 6. A wrong --version name must be caught by the formula's own test block.
if VERSION_NAME=x ruby "$tmp/check.rb" "$tmp/Formula/agent-fs.rb" AgentFs afs afs "$sha" >/dev/null 2>&1; then
  fail "the agent-fs test block accepted 'afs 2.1.0' though the binary prints 'agent-fs'"
fi

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "formula.sh: all checks passed"
