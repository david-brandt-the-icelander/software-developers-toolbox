# software-developers-toolbox
Call it utils, call it miscellaneous, or call it whatever... this is a repo for stuff that is useful in my daily work as a software developer

## Tools (macOS, Rust)
- [`fix-network`](macos/rust/fix-network/README.md): diagnose and repair Wi-Fi, DNS, routing and VPN.
- [`agent-fs`](macos/rust/agent-fs/README.md): JSON read/search/list/outline tool and MCP server for agents, with session caching (`afs`).
- [`afo`/`afod`](macos/rust/afod/README.md): file tool and MCP server for agents (find, read, outline, refs, safe edit) with a tiny C invoker, per-session dedupe and a persistent cache.
- [`free-disk-space`](macos/rust/free-disk-space/README.md): scan, compress, dedupe and clean up disk space.
- [`codeindexd`](macos/dotnet/codeindexd/README.md) (C#): MCP server that answers code questions for agents instead of grep and cat.

Install with Homebrew (no Gatekeeper prompt, `brew upgrade` for updates):

```bash
brew tap david-brandt-the-icelander/toolbox https://github.com/david-brandt-the-icelander/software-developers-toolbox
brew install david-brandt-the-icelander/toolbox/fix-network   # also: free-disk-space, agent-fs (afs)
```

Or download the universal binaries:

```bash
curl -fLO https://icelander.net/downloads/toolbox/latest/toolbox-macos-universal.tar.gz
curl -fLO https://icelander.net/downloads/toolbox/latest/toolbox-macos-universal.tar.gz.sha256
shasum -a 256 -c toolbox-macos-universal.tar.gz.sha256
tar xzf toolbox-macos-universal.tar.gz
```

A file downloaded with `curl` runs as is. Downloaded in a browser, an unsigned binary makes macOS say it "could not verify" it; after checking the checksum, run `xattr -d com.apple.quarantine afo afod afs fix-network free-disk-space`. Signing and notarization are optional ([`macos/release/`](macos/release/README.md)).

Or build with `cd macos/rust && cargo build --release`.

Linux tarballs with `afo` and `afod` (`toolbox-linux-x86_64.tar.gz`, `toolbox-linux-aarch64.tar.gz`) are next to the macOS one. `afo` is arm64-only on macOS; Intel Macs use `afod mcp` or `afod run`.

A specific version is at `https://icelander.net/downloads/toolbox/<tag>/`. The GitHub releases carry the same files.
