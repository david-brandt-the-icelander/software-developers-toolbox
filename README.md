# zazzle
zazzle is a launcher for coding agents and developers: one command, `zazzle` (or `z`), in front of a daemon that reads, searches and edits code with short output, and a set of tools for daily work on macOS.

Only `zazzle` and `z` go on your PATH. Every other tool runs through the launcher as `zazzle TOOL ARGS`, for example `z daemon restart` or `z fix-network diagnose`.

## Tools
All source is in [`src/macos/`](src/macos), one folder per tool:

- [`zazzle`](src/macos/zazzle/README.md): the launcher, a tiny C program. It sends commands to the daemon, starts the daemon when needed, and runs every other tool from `libexec/zazzle`.
- [`daemon`](src/macos/daemon/README.md): the file tool and MCP server behind `zazzle` (find, read, outline, refs, safe edit, C# structure from the indexer, `clean` for piped command output, `run` for builds and tests, and `zazzle proxy` to shrink another MCP server's results), with per-session dedupe and a persistent cache. `zazzle daemon install|restart|mcp ...`.
- [`shell-tools`](src/macos/shell-tools/README.md): JSON read/search/list/outline tool and MCP server for agents, with session caching. `zazzle shell-tools ...`.
- [`git-tools`](src/macos/git-tools/README.md): git with a commit flow and message editor. `zazzle git ...` (or `zazzle git-tools ...`).
- [`fix-network`](src/macos/fix-network/README.md): diagnose and repair Wi-Fi, DNS, routing and VPN. `zazzle fix-network ...`.
- [`free-disk-space`](src/macos/free-disk-space/README.md): scan, compress, dedupe and clean up disk space; `zazzle free-disk-space daemon install` schedules it.
- [`ai-agent`](src/macos/ai-agent/README.md): runs open models locally (`zazzle agent llm|prompt`, or `zazzle ai-agent ...`); downloaded on first use.
- [`ai-agent-rules`](src/macos/ai-agent-rules/README.md): the rule lister behind `zazzle agent get-rules`.
- [`csharp-indexer`](src/macos/csharp-indexer/README.md) (C#): the C# side of zazzle: a per-user daemon with a live index and Roslyn that answers zazzle's terse C# views and structure tools (overview, find-symbol, changed-since, git history), plus `init` and the redirect hook. `zazzle csharp-indexer ...`.
- [`csharp-minifier`](src/macos/csharp-minifier/README.md) (C#): organizes C# tests into one file per method under test and lists and renames identifiers; built and run from source.
- [`zazz-script`](src/macos/zazz-script): the packaging, engine and runner behind sandboxed zazz scripts (`zazzle zazz ...`).
- [`language-modules`](src/macos/language-modules): the tree-sitter grammars the daemon downloads.
- [`lempel-ziv-finite-state-entropy-compression`](src/macos/lempel-ziv-finite-state-entropy-compression/README.md): the vendored LZFSE library free-disk-space compresses with.
- [`cyberspace-generator`](https://github.com/david-brandt-the-icelander/cyberspace-explorer#cyberspace-generator): validates and maintains a repository's `.dev` files, the views [cyberspace-explorer.com](https://cyberspace-explorer.com) shows. Built in cyberspace-explorer; the release pinned in `release/cyberspace-generator.version` ships here. `zazzle cyberspace-generator ...`.

## Install
With Homebrew (no Gatekeeper prompt, `brew upgrade` for updates):

```bash
brew install zazzcat/z/zazzle
```

Homebrew links only `zazzle`; the tools stay in the formula's `libexec/zazzle`. It also links `z` to `zazzle`, unless something else already uses `z`: another program's `z` in Homebrew's `bin` or `/usr/local/bin`, or zoxide or `z.sh` installed with Homebrew. To keep `z` free anyway, before or after installing:

```bash
mkdir -p "$(brew --prefix)/etc/zazzle" && touch "$(brew --prefix)/etc/zazzle/no-z"
```

```bash
brew postinstall zazzle
```

To test what is about to be released, install the alpha channel instead; each release publishes it first, before its full build and tests finish (the two conflict, so unlink one before installing the other):

```bash
brew install zazzcat/z/zazzle-alpha   # or: curl -fLO https://icelander.net/downloads/zazzle/alpha/zazzle-macos-universal.tar.gz
```

The formula lives in the [`zazzcat/homebrew-z`](https://github.com/zazzcat/homebrew-z) tap; brew adds it on first use.

Or download the universal binaries:

```bash
curl -fLO https://icelander.net/downloads/zazzle/latest/zazzle-macos-universal.tar.gz
```

```bash
curl -fLO https://icelander.net/downloads/zazzle/latest/zazzle-macos-universal.tar.gz.sha256
```

```bash
shasum -a 256 -c zazzle-macos-universal.tar.gz.sha256
```

```bash
mkdir -p ~/.local/zazzle && tar xzf zazzle-macos-universal.tar.gz -C ~/.local/zazzle
```

The tarball holds `bin/zazzle`, `bin/z` (a link to it) and the tools in `libexec/zazzle/`; keep the two folders side by side and put `bin` on your PATH. The C# indexer is in `libexec/indexer/` gzipped for both CPU types: unpack the one for your Mac as `libexec/zazzle/csharp-indexer` (`gunzip -c libexec/indexer/csharp-indexer-arm64.gz > libexec/zazzle/csharp-indexer && chmod +x libexec/zazzle/csharp-indexer`). A file downloaded with `curl` runs as is. Downloaded in a browser, an unsigned binary makes macOS say it "could not verify" it; after checking the checksum, run `xattr -dr com.apple.quarantine ~/.local/zazzle`. Signing and notarization are optional ([`release/`](release/README.md)).

Or build with `cd src/macos && cargo build --release`, then `make -C zazzle`; a build finds its tools beside the launcher.

Linux tarballs with the launcher, the daemon and `csharp-indexer` (`zazzle-linux-x86_64.tar.gz`, `zazzle-linux-aarch64.tar.gz`), in the same layout, are next to the macOS one. `zazzle` is arm64-only on macOS; Intel Macs use `libexec/zazzle/daemon mcp` or `libexec/zazzle/daemon run`.

A specific version is at `https://icelander.net/downloads/zazzle/<tag>/`. The GitHub releases carry the same files.
