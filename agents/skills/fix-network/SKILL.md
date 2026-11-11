---
name: fix-network
description: Diagnose and repair a Mac's Wi-Fi, internet, DNS, routing or VPN problems. Use when the user reports no internet, slow/failed DNS, captive portal, or a VPN that will not connect. Wraps the `fix-network` Rust binary in macos/rust/fix-network.
---

# fix-network

Binary: `macos/rust/target/release/fix-network` (build: `cd macos/rust && cargo build --release`; macOS only).

## Steps
1. Read-only first: `macos/rust/target/release/fix-network diagnose --internal-host HOST[:PORT]`; the verdict names the failing layer.
2. Repair: `... fix` (flush DNS, clear gateway ARP, renew DHCP if DHCP, cycle Wi-Fi). Add flags only with evidence:
   - `--vpn` tests without scutil-managed VPNs then reconnects them.
   - `--fix-proxy` disables unreachable proxies.
   - `--allow-route-flush` is a last resort.
3. Cycling Wi-Fi starts a detached watchdog that re-enables Wi-Fi; manual rescue: `... recover`.
4. Logs/original settings: `~/.local/state/fix-network/<timestamp>/` (`diagnostic.log`, `original.json`, `recover.sh`).

## Rules
- Never reboot, start Docker, delete network prefs/credentials/profiles, set public DNS, or touch routers.
- Never ask for passwords in chat; privileged steps use the user's terminal `sudo`.
- Without `--internal-host`, report VPN verification as incomplete.
- Report cause as confirmed only with evidence; "it recovered" is not proof of cause.
