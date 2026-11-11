# fix-network

`fix-network` — diagnose and progressively repair macOS Wi-Fi, DNS, routing and VPN (single Rust binary).

```bash
fix-network diagnose --internal-host wiki.corp.example:443   # read-only
fix-network fix --vpn --internal-host wiki.corp.example:443
fix-network recover                                              # force Wi-Fi power on
```

Finds the Wi-Fi device/service (not assuming `en0`), tests gateway, captive portal, IP reachability, system-resolver DNS and HTTPS separately, names the failing layer, then repairs upward: flush DNS, clear gateway ARP, renew DHCP (DHCP services only), cycle Wi-Fi, and with flags: `--vpn` (test without scutil-managed VPNs, then reconnect), `--fix-proxy`, `--allow-route-flush`.

Logs and original settings go to `~/.local/state/fix-network/<timestamp>/` (`diagnostic.log`, `original.json`, `recover.sh`). Before the Wi-Fi radio is cycled, a detached watchdog is started that turns Wi-Fi back on by itself. No reboot, Docker, preference deletion, public-DNS substitution or router changes. Privileged steps use `sudo` (prompts in your terminal; never in chat).

Build: `cargo build --release -p fix-network` in `macos/rust` (binary in `target/release/`), or download a release. Unit-tested in CI on macOS and Linux; repair steps are untested on a real Mac.
