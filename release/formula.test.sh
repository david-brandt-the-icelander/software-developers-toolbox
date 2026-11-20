#!/usr/bin/env bash
# Test formula.sh and bottle.sh: valid Ruby, correct DSL values under a stand-in Formula class, the post-install steps run against a fake prefix, and consistency with release.yml.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failures=0
sha=$(printf '%064d' 7 | tr 0 a)
tools="daemon zazz-script-runner fix-network free-disk-space git-tools git-tools-commit git-tools-commit-message-editor ai-agent-rules cyberspace-generator"
bins="zazzle $tools"

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

# 3. Load the formula under a stand-in Homebrew Formula class and check every DSL value, install, the post-install steps and test.
cat >"$tmp/check.rb" <<'RUBY'
# Stand-in for Homebrew's Formula: records DSL values, installs into a fake prefix, runs the post-install steps and the test block.
require "digest"
require "fileutils"
require "pathname"
require "zlib"
HOMEBREW_PREFIX = Pathname(ENV.fetch("BREW"))
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
  # Records the steps of a post_install_steps block, each with the guards around it, as Homebrew's InstallSteps::DSL does.
  class Steps
    attr_reader :list
    def initialize = (@list = []) && (@guards = [])
    def run(command, **opts) = add(:run, command, opts)
    def remove(path, **opts) = add(:remove, path, opts)
    def symlink(source, target, **opts) = add(:symlink, [source, target], opts)
    def unless_path_exists(path, base: nil)
      @guards.push([path, base])
      yield
    ensure
      @guards.pop
    end
    def add(type, what, opts) = @list << { type:, what:, opts:, guards: @guards.dup }
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
    def install_symlink(links) = (@links ||= {}).merge!(links)
    def links = @links || {}
    def to_s = @path.to_s
    def /(other) = Dir_.new(@path / other)
  end
  attr_reader :bin, :commands
  def initialize
    @keg = Pathname(ENV.fetch("PREFIX"))
    @bin = Dir_.new(@keg / "bin")
    @libexec = {}
  end
  def libexec = Dir_.new(@keg / "libexec").tap { |d| subs = @libexec; d.define_singleton_method(:/) { |o| subs[o] ||= Dir_.new(path / o) } }
  def libexec_dir(name) = @libexec[name]
  def shell_output(cmd) = ((@commands ||= []) << cmd) && "#{cmd} 2.1.0\n"
  def assert_match(pattern, text) = (text.include?(pattern) or raise "#{pattern.inspect} not in #{text.inspect}")
end

# Resolves a step path against Homebrew's bases: the fake HOMEBREW_PREFIX, the formula's opt link, an absolute path.
def resolve(path, base, opt)
  case base
  when :homebrew_prefix then HOMEBREW_PREFIX / path
  when :opt_prefix then opt / path
  when nil then Pathname(path)
  else raise "unknown base #{base}"
  end
end

# Runs the link steps the way Homebrew's InstallSteps::Runner does, in order, each only when every guard holds.
def run_link_steps(steps, opt)
  steps.each do |s|
    next if s[:type] == :run
    next unless s[:guards].all? { |path, base| !resolve(path, base, opt).exist? }
    if s[:type] == :remove
      p = resolve(s[:what], s[:opts][:base], opt)
      p.delete if p.symlink? && p.readlink.to_s.include?(s[:opts].fetch(:symlink_target_contains))
    else
      source = resolve(s[:what][0], s[:opts][:source_base], opt)
      target = resolve(s[:what][1], s[:opts][:target_base], opt)
      target.dirname.mkpath
      FileUtils.rm_f(target) if s[:opts][:overwrite]
      File.symlink(source, target)
    end
  end
end

file, sha = ARGV
load file
f = Zazzle
v = f.values
keg = ENV.fetch("PREFIX")
FileUtils.rm_rf [keg, HOMEBREW_PREFIX.to_s]
repo = "https://github.com/david-brandt-the-icelander/zazzle"
raise "url #{v[:url]}" unless v[:url] == "#{repo}/releases/download/v2.1.0/zazzle-macos-universal.tar.gz"
raise "version #{v[:version]}" unless v[:version] == "2.1.0"
raise "sha256" unless v[:sha256] == sha
raise "license" unless v[:license] == "Unlicense"
raise "depends_on" unless v[:depends_on] == :macos
raise "homepage #{v[:homepage]}" unless v[:homepage] == repo
raise "desc" unless v[:desc].is_a?(String) && v[:desc].length.between?(20, 100) && !v[:desc].end_with?(".")
inst = f.new
inst.install
tools = ENV.fetch("TOOLS").split
raise "bin #{inst.bin.installed}" unless inst.bin.installed == ["bin/zazzle"] && inst.bin.links.empty?
raise "tools #{inst.libexec_dir("zazzle").installed}" unless inst.libexec_dir("zazzle").installed == tools.map { "libexec/zazzle/#{_1}" }
indexer = inst.libexec_dir("indexer").installed
raise "indexer files #{indexer}" unless indexer == %w[libexec/indexer/csharp-indexer-arm64.gz libexec/indexer/csharp-indexer-x64.gz]
raise "legacy post_install" if Zazzle.method_defined?(:post_install)
raise "caveats #{inst.caveats}" unless inst.caveats.include?("zazzle daemon restart") && inst.caveats.include?("etc/zazzle/no-z")
steps = Formula::Steps.new.tap { |s| s.instance_eval(&f.post_install_steps) }.list

# The indexer step, run for real against a fake keg and uname: it writes libexec/zazzle/csharp-indexer for this CPU.
runs = steps.select { _1[:type] == :run }
raise "run steps #{runs}" unless runs.length == 1 && runs[0][:what] == "/bin/sh" && runs[0][:guards].empty?
opts = runs[0][:opts]
raise "writable #{opts}" unless opts[:writable_base] == :libexec &&
                                 opts[:writable_paths] == %w[zazzle/csharp-indexer zazzle/csharp-indexer.tmp]
raise "args not literal strings #{opts[:args]}" unless opts[:args].all?(String) && opts[:args].none? { |a| a.include?(keg) }
FileUtils.mkdir_p ["#{keg}/libexec/indexer", "#{keg}/libexec/zazzle"]
%w[arm64 x64].each { |a| Zlib::GzipWriter.open("#{keg}/libexec/indexer/csharp-indexer-#{a}.gz") { |z| z.write "indexer-#{a}" } }
arch = ENV.fetch("ARCH")
fake = "#{keg}/fake-bin"
FileUtils.mkdir_p fake
File.write("#{fake}/uname", "#!/bin/sh\necho #{arch == "arm64" ? "arm64" : "x86_64"}\n")
File.chmod(0o755, "#{fake}/uname")
args = opts[:args].map { |a| a.gsub("{{libexec}}", "#{keg}/libexec") }
raise "post-install step failed" unless Kernel.system({ "PATH" => "#{fake}:#{ENV.fetch("PATH")}" }, "/bin/sh", *args)
indexer_bin = "#{keg}/libexec/zazzle/csharp-indexer"
raise "expanded #{File.read(indexer_bin).inspect}" unless File.read(indexer_bin) == "indexer-#{arch}"
raise "mode" unless File.stat(indexer_bin).mode & 0o777 == 0o755
raise "temporary file left" if File.exist?("#{indexer_bin}.tmp")

# The z link: made when free, kept as zazzle's own across reruns, never over another program's z, and gone on opt-out.
opt = HOMEBREW_PREFIX / "opt/zazzle"
z = HOMEBREW_PREFIX / "bin/z"
link = steps.find { _1[:type] == :symlink }
raise "z link #{link}" unless link && link[:opts][:remove_on_uninstall] && link[:opts][:overwrite]
run_link_steps(steps, opt)
raise "z not linked" unless z.symlink? && z.readlink == opt / "bin/zazzle"
run_link_steps(steps, opt)
raise "z not kept on rerun" unless z.symlink? && z.readlink == opt / "bin/zazzle"
alpha = HOMEBREW_PREFIX / "opt/zazzle-alpha"
run_link_steps(steps, alpha)
raise "the other channel's z not replaced" unless z.readlink == alpha / "bin/zazzle"
(HOMEBREW_PREFIX / "etc/zazzle").mkpath
FileUtils.touch HOMEBREW_PREFIX / "etc/zazzle/no-z"
run_link_steps(steps, opt)
raise "z kept after opting out" if z.symlink? || z.exist?
FileUtils.rm HOMEBREW_PREFIX / "etc/zazzle/no-z"
z.write "another program's z"
run_link_steps(steps, opt)
raise "replaced another program's z" unless !z.symlink? && z.read == "another program's z"
z.delete
(HOMEBREW_PREFIX / "Cellar/other/1/bin").mkpath
FileUtils.touch HOMEBREW_PREFIX / "Cellar/other/1/bin/z"
File.symlink(HOMEBREW_PREFIX / "Cellar/other/1/bin/z", z)
run_link_steps(steps, opt)
raise "replaced another formula's z link" unless z.readlink.to_s.include?("Cellar/other")
z.delete
%w[bin/zoxide etc/profile.d/z.sh].each do |other|
  (HOMEBREW_PREFIX / other).dirname.mkpath
  FileUtils.touch HOMEBREW_PREFIX / other
  run_link_steps(steps, opt)
  raise "linked z beside #{other}" if z.symlink?
  (HOMEBREW_PREFIX / other).delete
end
File.symlink("/nowhere", z)
run_link_steps(steps, opt)
raise "a dangling z was not replaced" unless z.readlink == opt / "bin/zazzle"

inst.instance_eval(&f.test_block)
raise "test ran #{inst.commands}" unless inst.commands.length == 10
raise "test runs a tool outside the launcher" unless inst.commands.all? { |c| c.include?("/bin/zazzle") || c.include?("/libexec/zazzle/") }
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
    ARCH=$arch PREFIX="$tmp/prefix" BREW="$tmp/brew" TOOLS="$tools" BOTTLES="${bottles:-}" ruby "$tmp/check.rb" "$file" "$sha" || fail "zazzle.rb failed the DSL check ($arch)"
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
mkdir -p "$tmp/flat" "$tmp/bins"
if bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/partial" >/dev/null 2>&1; then fail "bottle.sh accepted a missing launcher"; fi
for b in $bins; do printf '#!/bin/sh\n' >"$tmp/flat/$b"; chmod 0755 "$tmp/flat/$b"; done
for a in arm64 x64; do printf 'gz' >"$tmp/flat/csharp-indexer-$a.gz"; done
# pack.sh: bad input, a missing file or launcher, then the layout bottle.sh and the formula read.
if bash "$here/pack.sh" "$tmp/flat" "$tmp/p.tgz" >/dev/null 2>&1; then fail "pack.sh accepted no files"; fi
if bash "$here/pack.sh" "$tmp/flat" "$tmp/p.tgz" daemon nope >/dev/null 2>&1; then fail "pack.sh accepted a missing file"; fi
if bash "$here/pack.sh" "$tmp/flat" "$tmp/p.tgz" daemon >/dev/null 2>&1; then fail "pack.sh packed without the launcher"; fi
# shellcheck disable=SC2086
bash "$here/pack.sh" "$tmp/flat" "$tmp/pack.tgz" $bins csharp-indexer-arm64.gz csharp-indexer-x64.gz >/dev/null || fail "pack.sh"
packed=$(tar -tvzf "$tmp/pack.tgz")
grep -q '^l.* bin/z -> zazzle$' <<<"$packed" || fail "tarball bin/z is not a relative link to zazzle"
[ "$(grep -c ' bin/.' <<<"$packed")" = 2 ] || fail "tarball bin/ holds more than zazzle and z"
grep -q ' libexec/zazzle/daemon$' <<<"$packed" || fail "tarball has no libexec/zazzle/daemon"
grep -q ' libexec/indexer/csharp-indexer-x64.gz$' <<<"$packed" || fail "tarball has no libexec/indexer"
tar -xzf "$tmp/pack.tgz" -C "$tmp/bins" bin/zazzle
if bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/partial" >/dev/null 2>&1; then fail "bottle.sh accepted a missing binary"; fi
tar -xzf "$tmp/pack.tgz" -C "$tmp/bins" libexec/zazzle
chmod 0644 "$tmp/bins/libexec/zazzle/fix-network"
if bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/partial" >/dev/null 2>&1; then fail "bottle.sh accepted a missing indexer"; fi
tar -xzf "$tmp/pack.tgz" -C "$tmp/bins"
chmod 0644 "$tmp/bins/libexec/zazzle/fix-network"
bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/bottles" >"$tmp/bottle.out"
bash "$here/bottle.sh" v2.1.0 "$tmp/bins" "$tmp/again" >/dev/null
file="$tmp/bottles/zazzle-2.1.0.all.bottle.tar.gz"
[ -f "$file" ] || fail "no bottle"
grep -q "^$(sha256sum "$file" | cut -d' ' -f1)  zazzle-2.1.0.all.bottle.tar.gz$" "$tmp/bottle.out" || fail "printed sha256"
cmp -s "$file" "$tmp/again/$(basename "$file")" || fail "bottle is not reproducible"
listing=$(tar tzf "$file" | grep -v '/$' | sort | tr '\n' ' ')
want="zazzle/2.1.0/bin/zazzle "
for a in arm64 x64; do want="${want}zazzle/2.1.0/libexec/indexer/csharp-indexer-$a.gz "; done
# shellcheck disable=SC2086 # one name per word
for b in $(printf '%s\n' $tools | sort); do want="${want}zazzle/2.1.0/libexec/zazzle/$b "; done
[ "$listing" = "$want" ] || fail "contents $listing"
# Read each listing whole first: grep -q stops early, and tar's SIGPIPE would fail a pipeline under pipefail.
verbose=$(tar tvzf "$file")
grep -q "^-rwxr-xr-x 0/0 .* zazzle/2.1.0/libexec/zazzle/fix-network$" <<<"$verbose" || fail "binary not 0755 root-owned"
grep -q "^-rwxr-xr-x 0/0 .* zazzle/2.1.0/bin/zazzle$" <<<"$verbose" || fail "launcher not 0755 root-owned"
if grep -q '/bin/z$' <<<"$listing"; then fail "the bottle links z itself, which post-install decides"; fi
# Homebrew's own check (Bottle#extract_snapshot): exactly one NAME directory holding exactly one VERSION directory.
mkdir "$tmp/x" && tar xzf "$file" -C "$tmp/x"
if [ "$(ls -A "$tmp/x")" != "zazzle" ] || [ "$(ls -A "$tmp/x/zazzle")" != "2.1.0" ]; then fail "not NAME/VERSION only"; fi
[ "$(ls -A "$tmp/x/zazzle/2.1.0/bin")" = "zazzle" ] || fail "bin holds more than the launcher"

# 5. Formula with the bottle: it gets a bottle block pointing at its file, checked under the stand-in Formula.
bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/Formula" "$tmp/bottles" >/dev/null
bottles="$tmp/bottles"
check
bottles=""
mv "$tmp/bottles/zazzle-2.1.0.all.bottle.tar.gz" "$tmp/zazzle.bottle"
if bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/F2" "$tmp/bottles" >/dev/null 2>&1; then fail "formula.sh accepted a missing bottle"; fi
if bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/F3" "$tmp/no-such-dir" >/dev/null 2>&1; then fail "formula.sh accepted a missing bottle dir"; fi

# 6. The alpha channel: its own formula and class that conflicts with zazzle, an alpha tag, and a bottle named after it.
FORMULA=zazzle-alpha bash "$here/formula.sh" v2.1.0-alpha.3 "$sha" "$tmp/FA" >/dev/null || fail "alpha formula"
ruby -c "$tmp/FA/zazzle-alpha.rb" >/dev/null || fail "zazzle-alpha.rb is not valid Ruby"
grep -q '^class ZazzleAlpha < Formula$' "$tmp/FA/zazzle-alpha.rb" || fail "alpha class name"
grep -q 'conflicts_with "zazzle"' "$tmp/FA/zazzle-alpha.rb" || fail "alpha conflicts with zazzle"
grep -q 'version "2.1.0-alpha.3"' "$tmp/FA/zazzle-alpha.rb" || fail "alpha version"
grep -q 'brew postinstall zazzle-alpha' "$tmp/FA/zazzle-alpha.rb" || fail "alpha caveat names its own formula"
if grep -q conflicts_with "$tmp/Formula/zazzle.rb"; then fail "the stable formula names a conflict"; fi
FORMULA=zazzle-alpha bash "$here/bottle.sh" v2.1.0-alpha.3 "$tmp/bins" "$tmp/ab" >/dev/null || fail "alpha bottle"
grep -q '^zazzle-alpha/2.1.0-alpha.3/bin/zazzle$' <<<"$(tar -tzf "$tmp/ab/zazzle-alpha-2.1.0-alpha.3.all.bottle.tar.gz")" || fail "alpha bottle layout"
if FORMULA=other bash "$here/formula.sh" v2.1.0 "$sha" "$tmp/FB" >/dev/null 2>&1; then fail "unknown formula accepted"; fi

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "formula.sh: all checks passed"
