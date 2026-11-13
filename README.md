# software-developers-toolbox
Call it utils, call it miscellaneous, or call it whatever... this is a repo for stuff that is useful in my daily work as a software developer

## Tools (macOS, Rust)
- [`fix-network`](macos/rust/fix-network/README.md): diagnose and repair Wi-Fi, DNS, routing and VPN (also `zazzle fix-network ...`).
- [`agent-fs`](macos/rust/agent-fs/README.md): JSON read/search/list/outline tool and MCP server for agents, with session caching (`afs`).
- [`zazzle`/`zazzled`](macos/rust/zazzled/README.md): file tool and MCP server for agents (find, read, outline, refs, safe edit) with a tiny C invoker, per-session dedupe and a persistent cache.
- [`free-disk-space`](macos/rust/free-disk-space/README.md): scan, compress, dedupe and clean up disk space (also `zazzle free-disk-space ...`; `daemon install` schedules it).
- [`zazzle-csharp-indexer`](macos/dotnet/zazzle-csharp-indexer/README.md) (C#): code indexer (Roslyn for C#) for agents; `zazzled mcp` starts it on demand and relays its tools as `index_*`.

Install with Homebrew (no Gatekeeper prompt, `brew upgrade` for updates):

```bash
brew install zazzcat/z/zazzle   # zazzle, zazzled, zazzle-csharp-indexer, afs, free-disk-space, fix-network
```

The formula lives in the [`zazzcat/homebrew-z`](https://github.com/zazzcat/homebrew-z) tap; brew adds it on first use. If you tapped the old `toolbox` tap, run `brew untap david-brandt-the-icelander/toolbox`.

Or download the universal binaries:

```bash
curl -fLO https://icelander.net/downloads/zazzle/latest/zazzle-macos-universal.tar.gz
curl -fLO https://icelander.net/downloads/zazzle/latest/zazzle-macos-universal.tar.gz.sha256
shasum -a 256 -c zazzle-macos-universal.tar.gz.sha256
tar xzf zazzle-macos-universal.tar.gz
```

A file downloaded with `curl` runs as is. Downloaded in a browser, an unsigned binary makes macOS say it "could not verify" it; after checking the checksum, run `xattr -d com.apple.quarantine zazzle zazzled afs fix-network free-disk-space`. Signing and notarization are optional ([`macos/release/`](macos/release/README.md)).

Or build with `cd macos/rust && cargo build --release`.

Linux tarballs with `zazzle`, `zazzled` and `zazzle-csharp-indexer` (`zazzle-linux-x86_64.tar.gz`, `zazzle-linux-aarch64.tar.gz`) are next to the macOS one. `zazzle` is arm64-only on macOS; Intel Macs use `zazzled mcp` or `zazzled run`.

A specific version is at `https://icelander.net/downloads/zazzle/<tag>/`. The GitHub releases carry the same files.
