---
name: fix-network
description: Diagnose and repair a Mac's Wi-Fi, internet, DNS, routing, proxy or VPN problems. Use when the user reports no internet, slow/failed DNS, captive portal, a VPN that is connected but the intranet is unreachable, or broken networking after sleep. Wraps the `fix-network` Rust binary in src/macos/fix-network.
---

# fix-network

Binary: `fix-network`, or `zazzle fix-network ...` (same program, started from the `zazzle` binary's directory); built at `src/macos/target/release/fix-network` (build: `cd src/macos && cargo build --release`; macOS only).

## Steps
1. Quick check (read-only, about 2s, no root): `... status` prints one line (Wi-Fi, IP, DNS, internet, VPN); exit 0 healthy, 1 degraded; `--json` gives `tool`, `version`, `ok` and one object per area. Diagnose only when it is degraded or the user reports a problem it cannot see (intranet, proxy).
2. Full diagnosis (read-only): `... diagnose --internal-host HOST[:PORT]` (repeatable; add `-v` for tunnels/split DNS, `--json` to parse).
3. Each finding says `fixable` (with the exact `fix` command), `not fixable by this tool` or `reboot needed`. Relay that outcome as given.
4. Preview: `... fix --dry-run <same args>`. Repair: `... fix <same args>`. Add flags only with evidence from the diagnosis:
   - `--vpn` reconnects the scutil-managed VPN involved.
   - `--fix-proxy` disables unreachable proxies.
   - `--allow-route-flush` is a last resort for stale routes.
   - `--allow-remote` only if the user accepts losing an SSH session.
   - `--only wifi|dns|vpn|proxy` (repeatable or comma-separated) limits repair to those areas when the user wants nothing else touched; it does not replace `--vpn` or `--fix-proxy`.
5. A Wi-Fi cycle starts a watchdog that re-enables Wi-Fi; manual rescue: `networksetup -setairportpower <device> on`.
6. Logs/original settings: `~/.local/state/fix-network/<timestamp>/`.

## Rules
- Never reboot, start Docker, delete network prefs/credentials/profiles, set public DNS, or touch routers.
- Never ask for passwords in chat; privileged steps use the user's terminal `sudo`.
- Without `--internal-host`, report corporate DNS/service as not verified.
- Cisco Secure Client/AnyConnect is observed only via routes; the user reconnects it.
- Report cause as confirmed only with evidence; "it recovered" is not proof of cause.
