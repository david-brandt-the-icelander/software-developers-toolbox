# software-developers-toolbox
Call it utils, call it miscellaneous, or call it whatever... this is a repo for stuff that is useful in my daily work as a software developer

## Tools (macOS, Rust)
- [`fix-network`](macos/rust/fix-network/README.md): diagnose and repair Wi-Fi, DNS, routing and VPN.
- [`agent-fs`](macos/rust/agent-fs/README.md): JSON read/search/list/outline tool and MCP server for agents, with session caching (`afs`).
- [`free-disk-space`](macos/rust/free-disk-space/README.md): scan, compress, dedupe and clean up disk space.

Build with `cd macos/rust && cargo build --release`, or download the universal binaries:

```bash
curl -fLO https://icelander.net/downloads/toolbox/latest/toolbox-macos-universal.tar.gz
curl -fLO https://icelander.net/downloads/toolbox/latest/toolbox-macos-universal.tar.gz.sha256
shasum -a 256 -c toolbox-macos-universal.tar.gz.sha256
tar xzf toolbox-macos-universal.tar.gz
```

Releases are signed with a Developer ID and notarized by Apple ([`macos/release/`](macos/release/README.md)). A build released before signing was set up makes macOS say it "could not verify" the binary; after checking the checksum, run `xattr -d com.apple.quarantine afs fix-network free-disk-space`.

A specific version is at `https://icelander.net/downloads/toolbox/<tag>/`. The GitHub releases carry the same files.
