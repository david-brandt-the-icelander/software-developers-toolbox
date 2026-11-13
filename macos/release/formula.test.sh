#!/usr/bin/env bash
# Test formula.sh and bottle.sh: valid Ruby, correct DSL values under a stand-in Formula class, and consistency with release.yml.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0
sha=$(printf '%064d' 7 | tr 0 a)
bins="zazzle zazzled afs fix-network free-disk-space"
placeholder=$(printf '#!/bin/sh\necho "zazzle-csharp-indexer is not set up yet; run: brew postinstall zazzle" >&2\nexit 1\nX')
placeholder=${placeholder%X}

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

# 2. Generate the formula for a release.
out=$(bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/Formula")
case "$out" in *"for v2.1.0"*) ;; *) fail "summary line: $out" ;; esac
[ "$(find "$tmp/Formula" -name '*.rb' | wc -l)" -eq 1 ] || fail "wrote other than 1 formula"

# 3. Load the formula under a stand-in Homebrew Formula class and check every DSL value, install, the post-install step and test.
cat >"$tmp/check.rb" <<'RUBY'
# Stand-in for Homebrew's Formula: records DSL values, installs into a fake prefix, runs the post-install step and the test block.
require "digest"
require "fileutils"
require "pathname"
require "zlib"
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
    def post_install_steps(&block) = block ? (@steps = block) : @steps
  end
  # Records the steps of a post_install_steps block.
  class Steps
    attr_reader :list
    def run(command, **opts) = (@list ||= []) << [command, opts]
  end
  # Records a bottle block's root_url and sha256 entries.
  class BottleSpec
    attr_reader :url, :checksums
    def root_url(url) = @url = url
    def sha256(**tags) = (@checksums ||= []) << tags
  end
  # A directory that records the files installed into it.
  class Dir_
    attr_reader :installed, :path
    def initialize(path) = @path = Pathname(path)
    def install(*files) = (@installed ||= []).concat(files)
    def write(text) = (@path.dirname.mkpath; @path.write(text))
    def to_s = @path.to_s
    def /(other) = Dir_.new(@path / other)
  end
  attr_reader :bin, :libexec, :commands, :systems
  def initialize
    prefix = Pathname(ENV.fetch("PREFIX"))
    @bin = Dir_.new(prefix / "bin")
    @libexec_root = Dir_.new(prefix / "libexec")
    @installs = {}
  end
  def libexec = @libexec_root.tap { |d| d.define_singleton_method(:/) { |o| @installs ||= {}; (@installs[o] ||= Dir_.new(@path / o)) } }
  def shell_output(cmd) = ((@commands ||= []) << cmd) && "#{cmd.split("/").last.split.first == "afs" ? "agent-fs" : cmd.split("/").last.split.first} 2.1.0\n"
  def assert_match(pattern, text) = (text.include?(pattern) or raise "#{pattern.inspect} not in #{text.inspect}")
  def chmod(mode, path) = File.chmod(mode, path.to_s)
end

file, sha = ARGV
load file
f = Zazzle
v = f.values
prefix = ENV.fetch("PREFIX")
FileUtils.rm_rf prefix
repo = "https://github.com/david-brandt-the-icelander/software-developers-toolbox"
raise "url #{v[:url]}" unless v[:url] == "#{repo}/releases/download/v2.1.0/zazzle-macos-universal.tar.gz"
raise "version #{v[:version]}" unless v[:version] == "2.1.0"
raise "sha256" unless v[:sha256] == sha
raise "license" unless v[:license] == "Unlicense"
raise "depends_on" unless v[:depends_on] == :macos
raise "homepage #{v[:homepage]}" unless v[:homepage] == repo
raise "desc" unless v[:desc].is_a?(String) && v[:desc].length.between?(20, 100) && !v[:desc].end_with?(".")
inst = f.new
inst.install
want = %w[zazzle zazzled afs fix-network free-disk-space]
raise "installed #{inst.bin.installed}" unless inst.bin.installed == want
indexer = inst.libexec / "indexer"
raise "indexer files #{indexer.installed}" unless indexer.installed == %w[zazzle-csharp-indexer-arm64.gz zazzle-csharp-indexer-x64.gz]
# The placeholder brew links at pour time, then the one post-install step run for real against a fake keg and uname.
raise "legacy post_install" if Zazzle.method_defined?(:post_install)
indexer_bin = "#{prefix}/bin/zazzle-csharp-indexer"
raise "placeholder" unless File.read(indexer_bin) == ENV.fetch("PLACEHOLDER") && File.stat(indexer_bin).mode & 0o777 == 0o755
steps = Formula::Steps.new.tap { |s| s.instance_eval(&f.post_install_steps) }.list
raise "post_install_steps #{steps}" unless steps.length == 1 && steps[0][0] == "/bin/sh"
opts = steps[0][1]
raise "writable #{opts}" unless opts[:writable_base] == :bin &&
                                 opts[:writable_paths] == %w[zazzle-csharp-indexer zazzle-csharp-indexer.tmp]
raise "args not literal strings #{opts[:args]}" unless opts[:args].all?(String) && opts[:args].none? { |a| a.include?(prefix) }
FileUtils.mkdir_p "#{prefix}/libexec/indexer"
%w[arm64 x64].each { |a| Zlib::GzipWriter.open("#{prefix}/libexec/indexer/zazzle-csharp-indexer-#{a}.gz") { |z| z.write "indexer-#{a}" } }
arch = ENV.fetch("ARCH")
fake = "#{prefix}/fake-bin"
FileUtils.mkdir_p fake
File.write("#{fake}/uname", "#!/bin/sh\necho #{arch == "arm64" ? "arm64" : "x86_64"}\n")
File.chmod(0o755, "#{fake}/uname")
args = opts[:args].map { |a| a.gsub("{{libexec}}", "#{prefix}/libexec").gsub("{{bin}}", "#{prefix}/bin") }
raise "post-install step failed" unless Kernel.system({ "PATH" => "#{fake}:#{ENV.fetch("PATH")}" }, "/bin/sh", *args)
raise "expanded #{File.read(indexer_bin).inspect}" unless File.read(indexer_bin) == "indexer-#{arch}"
raise "mode" unless File.stat(indexer_bin).mode & 0o777 == 0o755
raise "temporary file left" if File.exist?("#{indexer_bin}.tmp")
inst.instance_eval(&f.test_block)
need = %w[zazzle zazzled fix-network free-disk-space afs zazzle-csharp-indexer]
raise "test ran #{inst.commands}" unless inst.commands.length == need.length
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
  path = File.join(bottles, "zazzle-#{v[:version]}.all.bottle.tar.gz")
  raise "Homebrew would fetch #{File.basename(path)}, which bottle.sh did not write" unless File.exist?(path)
  raise "bottle sha256 mismatch" unless entry[:all] == Digest::SHA256.file(path).hexdigest
end
puts "ok"
RUBY

# Check the formula for both CPU types: valid Ruby, DSL values, and that release.yml builds every binary.
check() {
  local file="$tmp/Formula/zazzle.rb" arch
  ruby -c "$file" >/dev/null || fail "zazzle.rb is not valid Ruby"
  for arch in arm64 x64; do
    ARCH=$arch PREFIX="$tmp/prefix" PLACEHOLDER="$placeholder" BOTTLES="${bottles:-}" ruby "$tmp/check.rb" "$file" "$sha" >/dev/null || fail "zazzle.rb failed the DSL check ($arch)"
  done
  for b in $bins; do
    grep -q "for bin in .*\b$b\b" "$root/.github/workflows/release.yml" || grep -q "dist/$b\b" "$root/.github/workflows/release.yml" || fail "release.yml does not build $b"
  done
}
check

# 4. Bottle: bad input, then one reproducible bottle in exactly the layout Homebrew pours.
for args in "" "2.1.0 $tmp/bins $tmp/b" "v2.1.0 $tmp/none $tmp/b"; do
  # shellcheck disable=SC2086
  if bash "$here/bottle.sh" $args >/dev/null 2>&1; then fail "bottle.sh accepted bad input: '$args'"; fi
done
mkdir -p "$tmp/bins"
printf '#!/bin/sh\n' >"$tmp/bins/zazzle"
if bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/partial" >/dev/null 2>&1; then fail "bottle.sh accepted a missing binary"; fi
for b in $bins; do printf '#!/bin/sh\n' >"$tmp/bins/$b"; done
chmod 0644 "$tmp/bins/afs"
if bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/partial" >/dev/null 2>&1; then fail "bottle.sh accepted a missing indexer"; fi
for a in arm64 x64; do printf 'gz' >"$tmp/bins/zazzle-csharp-indexer-$a.gz"; done
bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/bottles" >"$tmp/bottle.out"
bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/again" >/dev/null
file="$tmp/bottles/zazzle-2.1.0.all.bottle.tar.gz"
[ -f "$file" ] || fail "no bottle"
grep -q "^$(sha256sum "$file" | cut -d' ' -f1)  zazzle-2.1.0.all.bottle.tar.gz$" "$tmp/bottle.out" || fail "printed sha256"
cmp -s "$file" "$tmp/again/$(basename "$file")" || fail "bottle is not reproducible"
listing=$(tar tzf "$file" | grep -v '/$' | tr '\n' ' ')
want="zazzle/2.1.0/bin/afs zazzle/2.1.0/bin/fix-network zazzle/2.1.0/bin/free-disk-space zazzle/2.1.0/bin/zazzle zazzle/2.1.0/bin/zazzle-csharp-indexer zazzle/2.1.0/bin/zazzled zazzle/2.1.0/libexec/indexer/zazzle-csharp-indexer-arm64.gz zazzle/2.1.0/libexec/indexer/zazzle-csharp-indexer-x64.gz "
[ "$listing" = "$want" ] || fail "contents $listing"
tar tvzf "$file" | grep -q "^-rwxr-xr-x 0/0 .* zazzle/2.1.0/bin/afs$" || fail "binary not 0755 root-owned"
# Homebrew's own check (Bottle#extract_snapshot): exactly one NAME directory holding exactly one VERSION directory.
mkdir "$tmp/x" && tar xzf "$file" -C "$tmp/x"
[ "$(ls -A "$tmp/x")" = "zazzle" ] && [ "$(ls -A "$tmp/x/zazzle")" = "2.1.0" ] || fail "not NAME/VERSION only"
[ "$(cat "$tmp/x/zazzle/2.1.0/bin/zazzle-csharp-indexer"; printf X)" = "${placeholder}X" ] || fail "bottle placeholder differs from the formula's"
tar tvzf "$file" | grep -q "^-rwxr-xr-x 0/0 .* zazzle/2.1.0/bin/zazzle-csharp-indexer$" || fail "placeholder not 0755"

# 5. Formula with the bottle: it gets a bottle block pointing at its file, checked under the stand-in Formula.
bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/Formula" "$tmp/bottles" >/dev/null
bottles="$tmp/bottles"
check
bottles=""
mv "$tmp/bottles/zazzle-2.1.0.all.bottle.tar.gz" "$tmp/zazzle.bottle"
if bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/F2" "$tmp/bottles" >/dev/null 2>&1; then fail "formula.sh accepted a missing bottle"; fi
if bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/F3" "$tmp/no-such-dir" >/dev/null 2>&1; then fail "formula.sh accepted a missing bottle dir"; fi

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "formula.sh: all checks passed"
