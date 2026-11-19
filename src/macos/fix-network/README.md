# fix-network

`fix-network` diagnoses and repairs macOS network problems: link, public internet, VPN, routing, corporate DNS and services, and proxies. Single Rust binary, three modes.

```bash
zazzle fix-network status                                                  # one line in about 2s; exit 0 healthy, 1 degraded
zazzle fix-network diagnose --internal-host wiki.corp.example:443          # read-only, no root
zazzle fix-network fix --dry-run --internal-host wiki.corp.example:443     # show the plan, change nothing
zazzle fix-network fix --vpn --internal-host wiki.corp.example:443         # repair, then verify again
```

## Status (`status`)
A quick health line for a shell prompt or a script, read-only and without root, for example `network ok | Wi-Fi on | IP 192.168.1.5 (en0) | DNS ok | internet ok | VPN Work (utun4)`. It uses the same probes as `diagnose` with short limits (2s per command, 1s for the name lookup and each connection) and skips the slow ones (gateway ping, captive portal, HTTPS, proxies, internal hosts), so it answers in about 2 seconds even when the network is down.

| Area | Healthy when |
|---|---|
| Wi-Fi | the radio of the service in use is on (`not used` on a wired service) |
| IP | the service in use has an IPv4 or global IPv6 address |
| DNS | `www.apple.com` resolves through the system resolver |
| Internet | TCP 443 to 1.1.1.1 (or its IPv6 address) or to `www.apple.com` connects |
| VPN | no scutil-managed VPN is stuck between states (`connected`, a third-party `tunnel` or `none` are healthy) |

Exit status: 0 healthy, 1 degraded, 2 not macOS. `--json` prints one object: `tool` (`fix-network`), `version`, `ok`, and `wifi` (`ok`, `state`, `device`), `ip` (`ok`, `interface`, `service`, `ipv4`, `ipv6_global`), `dns` (`ok`), `internet` (`ok`) and `vpn` (`ok`, `state`, `name`, `interface`).

## Checks
Each run reports seven dimensions separately. The states are `HEALTHY`, `FAILED`, `NONE`, `UNKNOWN`, `NOT TESTED` and `UNSUPPORTED`. A check that did not run is never reported as healthy.

| Check | What it verifies |
|---|---|
| Local network | network service in use (Wi-Fi or wired, not assuming `en0`), radio power, IPv4/IPv6 address, gateway |
| Public internet | captive portal, IP path (IPv4, else IPv6), system-resolver DNS, HTTPS |
| VPN connection | scutil-managed VPNs: connected, stuck, stale (interface gone), several tunnels claiming the default route; other tunnels observed through their routes |
| Routing | default route, split vs full tunnel, routes left on interfaces that are down, internal hosts routed around the tunnel |
| Corporate DNS | each `--internal-host` name; split-DNS resolvers left pointing at a dead tunnel |
| Corporate service | TCP to each `--internal-host HOST:PORT` |
| Proxy | enabled system HTTP/HTTPS proxies and PAC file are reachable |

Public internet being healthy does not mean the VPN is healthy. If you pass `--internal-host`, every check it covers decides the result and the exit status.

When an internal host fails, the report says which of these it is:
- the name does not resolve (the corporate DNS server is down, the VPN set up no split DNS, or the local resolver is faulty, found by asking the server directly with `dig`)
- it resolves to `0.0.0.0` or loopback (a DNS filter or `/etc/hosts` entry)
- it is routed around the tunnel
- TCP fails through the tunnel
- the service itself is down

## Outcomes
Every finding ends in one of three outcomes:
- **fixable**: the repair steps, and the `fix` command line with any flags it needs.
- **not fixable by this tool**: what to do yourself (captive portal sign-in, router or ISP outage, VPN sign-in/MFA, service down, conflicting VPN clients).
- **reboot needed**: only where a reboot is the known fix:
  - configd hung (`networksetup`/`scutil` time out)
  - Wi-Fi hardware missing, or the radio will not power on
  - the system resolver fails after a flush while the DNS server answers directly
  - stale routes or split-DNS resolvers that survive quitting the VPN client

If the Mac woke from sleep in the last hour, VPN findings are tagged with how long ago it woke, because the VPN session is probably stale.

## Repair (`fix`)
Repairs adapt to what was found. The tool runs the least disruptive allowed step, takes a fresh snapshot, assesses again, and stops when everything is healthy or nothing more is allowed. It then waits 15s and verifies every dimension again, including the internal hosts.

Order: turn Wi-Fi on, flush DNS, clear the gateway ARP entry, disable unreachable proxies, reconnect the VPN, renew DHCP, cycle Wi-Fi, flush routes. Wi-Fi is cycled only when the local link or upstream path is at fault, never for a VPN-only fault.

Every command's exit status, and any `networksetup` error text, is checked. Each step is reported as `ok` or `FAILED` with the reason.

| Flag | Allows |
|---|---|
| `--vpn` | `scutil --nc stop/start` of the VPN involved, verified until `Connected` |
| `--fix-proxy` | disabling unreachable web/secure proxies on the active service (needs sudo) |
| `--allow-route-flush` | `route -n flush`, a last resort for stale routes |
| `--allow-remote` | over SSH, steps that can cut the session: DHCP renew, Wi-Fi cycle, VPN reconnect, route flush |
| `--force` | first runs flush DNS, clear ARP, renew DHCP and cycle Wi-Fi even if healthy |
| `--dry-run` | prints the diagnosis and plan only |
| `--only AREA` | repairs only these areas (repeatable or comma-separated): `wifi` (turn Wi-Fi on, clear ARP, renew DHCP, cycle Wi-Fi), `dns` (flush DNS), `vpn` (reconnect the VPN, flush routes), `proxy` (disable unreachable proxies). Other steps are listed as `needs --only AREA`. It only narrows: `--vpn`, `--fix-proxy` and `--allow-route-flush` are still needed |

Both modes take `--internal-host HOST[:PORT]` (repeatable; `[v6]:port` works), `--json` and `-v`/`--verbose`. Exit status: 0 healthy, 1 problems remain, 2 not macOS.

## Permissions and safety
- `status`, `diagnose` and `--dry-run` change nothing and need no root; `status` writes no log either.
- DNS flush, ARP clear, DHCP renew, proxy changes and route flush use `sudo`. It prompts in your terminal, and fails rather than prompting when there is no terminal.
- Before cycling the radio, a detached watchdog (`recover.sh`) is started that turns Wi-Fi back on after 45s. Manual rescue: `networksetup -setairportpower en0 on` (use your device; the log prints it).
- Never done: reboot, router changes, deleting network preferences, VPN profiles or keychains, substituting public DNS, editing scoped DNS or MDM-managed settings, killing VPN processes, storing credentials.
- Logs and original settings, with secret-looking lines dropped, go to `~/.local/state/fix-network/<timestamp>/`: `diagnostic.log`, `original.json`, `recover.sh`.

## Limitations
- VPN control covers only VPNs listed by `scutil --nc` (built-in IKEv2/IPSec/L2TP and Network-Extension VPNs such as WireGuard). Other clients, including Cisco Secure Client/AnyConnect, are only observed through their tunnel routes. Reconnect them yourself; `--vpn` cannot sign in for you when MFA or SSO is required.
- The tests run the tool against a scripted Mac, not a real one: how it is tested is in [src/.dev/verification.json](src/.dev/verification.json), and what that leaves unchecked in [src/.dev/limitations.json](src/.dev/limitations.json).
- `networksetup -getairportnetwork` is unreliable on recent macOS, so Wi-Fi association is judged by the address, not the SSID.

Planned work, such as controlling AnyConnect: [src/.dev/backlog.json](src/.dev/backlog.json).

## Troubleshooting examples
```bash
zazzle fix-network diagnose -v --internal-host wiki.corp.example:443   # VPN up but intranet dead after sleep?
zazzle fix-network fix --vpn --internal-host wiki.corp.example:443     # reconnect the VPN, re-verify the intranet
zazzle fix-network diagnose --json | jq '.findings'                     # machine-readable
```

Build: `cargo build --release -p fix-network` in `src/macos` (binary in `target/release/`), or download a release: `https://icelander.net/downloads/zazzle/latest/zazzle-macos-universal.tar.gz` (with a `.sha256` next to it; also on the GitHub releases). Or with Homebrew: `brew install zazzcat/z/zazzle` (installs every tool here). A browser download of an unsigned build makes macOS say it "could not verify" the binary; run `xattr -d com.apple.quarantine shell-tools fix-network free-disk-space` after unpacking.
