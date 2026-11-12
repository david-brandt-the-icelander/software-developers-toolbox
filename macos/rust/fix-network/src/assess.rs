// Turn a facts snapshot into per-dimension health and findings with remedies.
use std::net::IpAddr;

use crate::corp::{corp_dns, corp_service, proxy};
use crate::facts::{primary_nameserver, Facts, Internal};
pub use crate::model::{Check, Finding, Remedy, Report, State, Step};
use crate::parse::{is_private, is_tunnel};

// Seconds after a wake during which VPN failures are flagged as likely stale.
const RECENT_WAKE: u64 = 3600;

// Build a check result.
pub fn check(name: &'static str, state: State, detail: impl Into<String>) -> Check {
    Check { name, state, detail: detail.into() }
}

// Assessment state: the snapshot, steps already tried and findings so far.
pub struct Ctx<'a> {
    pub f: &'a Facts,
    tried: &'a [Step],
    out: Vec<Finding>,
}

impl Ctx<'_> {
    // Record a finding.
    pub fn add(&mut self, code: &'static str, summary: impl Into<String>, remedy: Remedy) {
        self.out.push(Finding { code, summary: summary.into(), remedy });
    }

    // Offer the untried candidate steps, or the fallback once all were tried.
    pub fn steps(&self, candidates: Vec<Step>, fallback: Remedy) -> Remedy {
        let left: Vec<Step> = candidates.into_iter().filter(|s| !self.tried.contains(s)).collect();
        if left.is_empty() {
            fallback
        } else {
            Remedy::Steps(left)
        }
    }

    // Return physical-link repairs: ARP (optional), DHCP renewal and Wi-Fi cycling.
    pub fn phys_steps(&self, arp: bool) -> Vec<Step> {
        let f = self.f;
        let mut out = Vec::new();
        if arp && f.gateway.is_some() {
            out.push(Step::ClearArp);
        }
        if f.dhcp {
            out.push(Step::RenewDhcp);
        }
        if f.phys.as_ref().is_some_and(|p| p.is_wifi()) {
            out.push(Step::CycleWifi);
        }
        out
    }

    // Offer a reconnect of the scutil VPN involved, else manual advice.
    pub fn vpn_fix(&self, manual: &str) -> Remedy {
        let f = self.f;
        let stuck = f.vpns.iter().find(|v| v.state != "Disconnected");
        let Some(v) = f.connected_vpn().or(stuck) else {
            return Remedy::Manual(manual.to_string());
        };
        let after = format!(
            "Reconnecting \"{}\" did not help: reconnect it from the menu bar (it may need sign-in or MFA) \
             or ask the VPN admin.",
            v.name
        );
        self.steps(vec![Step::ReconnectVpn(v.name.clone())], Remedy::Manual(after))
    }

    // Offer to start the only configured scutil VPN, else advise connecting one.
    pub fn vpn_connect(&self) -> Remedy {
        let manual = Remedy::Manual("Connect your VPN, then run fix-network again.".into());
        match self.f.vpns.as_slice() {
            [only] => self.steps(vec![Step::ReconnectVpn(only.name.clone())], manual),
            _ => manual,
        }
    }
}

// Describe the physical service for messages.
fn dev_label(f: &Facts) -> String {
    f.phys.as_ref().map_or("?".into(), |p| format!("{} ({})", p.device, p.name))
}

// Check the physical link: hardware, radio, address and gateway.
fn local(cx: &mut Ctx) -> Check {
    let name = "Local network";
    let f = cx.f;
    if f.config_hung {
        let text = "Reboot: the macOS network configuration service (configd) is not responding.";
        cx.add("config-hung", "networksetup/scutil time out", Remedy::Reboot(text.into()));
        return check(name, State::Failed, "network configuration commands time out");
    }
    let Some(phys) = &f.phys else {
        let text = "Connect a cable or adapter, or enable a network service in System Settings > Network.";
        cx.add("no-interface", "No enabled network hardware port", Remedy::Manual(text.into()));
        return check(name, State::Failed, "no network hardware port");
    };
    let dev = dev_label(f);
    if f.wifi_missing {
        let text = "Reboot; if Wi-Fi is still missing afterwards, the hardware or driver needs service.";
        cx.add(
            "wifi-missing",
            format!("Wi-Fi hardware {dev} is not visible to the system"),
            Remedy::Reboot(text.into()),
        );
        return check(name, State::Failed, format!("{dev} missing"));
    }
    if f.wifi_power == Some(false) {
        let fallback = Remedy::Reboot("The Wi-Fi radio does not power on: reboot.".into());
        let remedy = cx.steps(vec![Step::WifiOn], fallback);
        cx.add("wifi-off", format!("Wi-Fi {dev} is powered off"), remedy);
        return check(name, State::Failed, format!("{dev} powered off"));
    }
    if f.link.ipv4.is_none() && !f.link.ipv6_global {
        let text = "No address from the network: its DHCP may be down, the Wi-Fi password may have changed, \
                    or the network refuses this device. Rejoin the network or restart the router.";
        let remedy = cx.steps(cx.phys_steps(false), Remedy::Manual(text.into()));
        cx.add("no-address", format!("{dev} has no usable IP address (none or self-assigned)"), remedy);
        return check(name, State::Failed, format!("{dev} no address"));
    }
    let gw = f.gateway.clone().unwrap_or_default();
    if f.gateway_ok == Some(false) && !f.public.ip_ok {
        let text = "Router unreachable: check signal, cable or access point, then restart the router \
                    (not done automatically).";
        let remedy = cx.steps(cx.phys_steps(true), Remedy::Manual(text.into()));
        cx.add("gateway-unreachable", format!("Router {gw} does not answer on {dev}"), remedy);
        return check(name, State::Failed, format!("router {gw} unreachable"));
    }
    let addr = f.link.ipv4.clone().unwrap_or("IPv6 only".into());
    let gw = if gw.is_empty() { String::new() } else { format!(", gateway {gw}") };
    check(name, State::Healthy, format!("{} {addr}{gw}", phys.device))
}

// Check public internet reachability in layers.
fn public(cx: &mut Ctx, local_ok: bool) -> Check {
    let name = "Public internet";
    let f = cx.f;
    let p = &f.public;
    if !p.captive && p.ip_ok && p.dns_ok && p.https_ok {
        let mut detail = "IP, DNS and HTTPS ok".to_string();
        if p.ip6_ok == Some(false) {
            detail.push_str("; IPv6 path fails (IPv4 used)");
        }
        if f.full_tunnel() {
            detail.push_str(&format!("; via tunnel {}", f.public_if.as_deref().unwrap_or("?")));
        }
        return check(name, State::Healthy, detail);
    }
    if !local_ok {
        return check(name, State::Failed, "unreachable: local network failed");
    }
    if p.captive {
        let text = "Open a browser and complete the network's sign-in page.";
        cx.add("captive-portal", "A captive portal intercepts web traffic", Remedy::Manual(text.into()));
        return check(name, State::Failed, "captive portal");
    }
    if !p.ip_ok && !p.https_ok {
        if f.full_tunnel() {
            let tun = f.public_if.clone().unwrap_or_default();
            let remedy = cx.vpn_fix(&format!("Reconnect or disconnect the VPN client that owns {tun}."));
            cx.add(
                "vpn-full-tunnel-dead",
                format!("All traffic goes into VPN tunnel {tun} but nothing gets through"),
                remedy,
            );
        } else {
            let text =
                "Likely an ISP or router outage: restart the router or contact the ISP (not fixable here).";
            let remedy = cx.steps(cx.phys_steps(true), Remedy::Manual(text.into()));
            cx.add("upstream", "The router answers but the internet does not", remedy);
        }
        return check(name, State::Failed, "no IP path to the internet");
    }
    if !p.dns_ok {
        dns_finding(cx);
        return check(name, State::Failed, "DNS lookups fail");
    }
    if p.https_ok {
        return check(name, State::Healthy, "DNS and HTTPS ok; the raw-IP probe to 1.1.1.1:443 is blocked");
    }
    let text = "Something blocks HTTPS: a firewall, TLS-inspecting security software, a wrong clock, \
                or network filtering (not fixable here).";
    let remedy = cx.steps(vec![Step::FlushDns], Remedy::Manual(text.into()));
    cx.add("https", "HTTPS fails although IP and DNS work", remedy);
    check(name, State::Failed, "HTTPS fails")
}

// Explain a public DNS failure using the direct-to-server query.
fn dns_finding(cx: &mut Ctx) {
    let f = cx.f;
    let ns = primary_nameserver(&f.resolvers).unwrap_or("?").to_string();
    if f.public.direct_dns_ok == Some(true) {
        let text = "The system resolver (mDNSResponder) still fails after a flush: reboot.";
        let remedy = cx.steps(vec![Step::FlushDns], Remedy::Reboot(text.into()));
        cx.add(
            "dns-resolver",
            format!("DNS server {ns} answers directly but the system resolver fails"),
            remedy,
        );
        return;
    }
    let mut steps = vec![Step::FlushDns];
    match f.connected_vpn() {
        Some(v) if f.full_tunnel() => steps.push(Step::ReconnectVpn(v.name.clone())),
        _ if f.dhcp => steps.push(Step::RenewDhcp),
        _ => {}
    }
    let text =
        "The configured DNS servers do not answer: check the router or VPN DNS, or ask the network admin. \
                Public DNS is never substituted automatically.";
    let remedy = cx.steps(steps, Remedy::Manual(text.into()));
    cx.add("dns-server", format!("DNS server {ns} does not answer"), remedy);
}

// Check VPN client state: stuck, stale, conflicting, connected or absent.
fn vpn(cx: &mut Ctx) -> Check {
    let name = "VPN connection";
    let f = cx.f;
    if let Some(v) = f.vpns.iter().find(|v| v.state == "Connecting" || v.state == "Disconnecting") {
        let remedy = cx
            .vpn_fix("Cancel and reconnect it from the menu bar; quit and reopen its app if it stays stuck.");
        cx.add("vpn-stuck", format!("VPN \"{}\" is stuck {}", v.name, v.state.to_lowercase()), remedy);
        return check(name, State::Failed, format!("\"{}\" stuck {}", v.name, v.state));
    }
    let gone =
        f.connected_vpn().and_then(|v| v.iface.clone().filter(|i| !f.up_ifaces.contains(i)).map(|i| (v, i)));
    if let Some((v, i)) = gone {
        let remedy = cx.vpn_fix("Disconnect and reconnect the VPN from the menu bar.");
        let summary =
            format!("VPN \"{}\" reports Connected but its interface {i} is gone (stale session)", v.name);
        cx.add("vpn-stale", summary, remedy);
        return check(name, State::Failed, format!("\"{}\" stale", v.name));
    }
    let defaults = f.tunnel_defaults();
    if defaults.len() > 1 {
        let text = "Two VPN clients are active at once: disconnect or quit one of them.";
        cx.add(
            "vpn-conflict",
            format!("Several tunnels claim the default route: {}", defaults.join(", ")),
            Remedy::Manual(text.into()),
        );
        return check(name, State::Failed, format!("conflicting tunnels {}", defaults.join(", ")));
    }
    let tunnels = f.tunnels();
    let mode = if f.full_tunnel() { "full tunnel" } else { "split tunnel" };
    if let Some(v) = f.connected_vpn() {
        let iface = v.iface.as_deref().unwrap_or("?");
        let mut detail = format!("\"{}\" connected on {iface} ({mode})", v.name);
        let others: Vec<&String> =
            tunnels.iter().filter(|t| Some(t.as_str()) != v.iface.as_deref()).collect();
        if !others.is_empty() {
            detail.push_str(&format!(
                "; other tunnels: {}",
                others.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        return check(name, State::Healthy, detail);
    }
    if !tunnels.is_empty() {
        let detail = format!(
            "tunnel {} active ({mode}); client not managed by scutil, state unreadable",
            tunnels.join(", ")
        );
        return check(name, State::Unsupported, detail);
    }
    if let Some(v) = f.vpns.iter().find(|v| v.state != "Disconnected") {
        return check(name, State::Unknown, format!("\"{}\" in unrecognized state {}", v.name, v.state));
    }
    check(name, State::Absent, format!("no VPN connected ({} scutil VPN services configured)", f.vpns.len()))
}

// Tell whether a private internal address bypasses an active tunnel.
pub fn wrong_route(f: &Facts, i: &Internal) -> Option<(IpAddr, String)> {
    let ip = *i.addrs.as_ref().ok()?.first()?;
    let via = i.route_if.clone()?;
    let bypass = is_private(&ip) && !f.tunnels().is_empty() && !is_tunnel(&via);
    bypass.then_some((ip, via))
}

// Check routes: default route, stale routes and internal-host routes.
fn routing(cx: &mut Ctx) -> Check {
    let name = "Routing";
    let f = cx.f;
    let mut failed = Vec::new();
    let has_addr = f.link.ipv4.is_some() || f.link.ipv6_global;
    if f.default_if.is_none() && has_addr {
        let text = "No router is configured: check the network's DHCP or the manual IP settings.";
        let remedy =
            cx.steps(if f.dhcp { vec![Step::RenewDhcp] } else { vec![] }, Remedy::Manual(text.into()));
        cx.add(
            "no-default-route",
            format!("No default route although {} has an address", dev_label(f)),
            remedy,
        );
        failed.push("no default route".to_string());
    }
    let mut stale_ifs: Vec<String> = f.stale_routes().iter().map(|r| r.netif.clone()).collect();
    stale_ifs.dedup();
    if !stale_ifs.is_empty() {
        let mut steps = Vec::new();
        if let Some(v) = f.connected_vpn() {
            steps.push(Step::ReconnectVpn(v.name.clone()));
        }
        steps.push(Step::FlushRoutes);
        let text = "Stale routes persist: quit the VPN client that created them; reboot if they remain.";
        let remedy = cx.steps(steps, Remedy::Reboot(text.into()));
        let summary = format!(
            "Routes point at interfaces that are down: {} (left by an old VPN/network)",
            stale_ifs.join(", ")
        );
        cx.add("stale-routes", summary, remedy);
        failed.push(format!("stale routes via {}", stale_ifs.join(", ")));
    }
    for i in &f.internal {
        let Some((ip, via)) = wrong_route(f, i) else {
            continue;
        };
        let remedy =
            cx.vpn_fix(&format!("The VPN did not install a route for {ip}: reconnect the VPN client."));
        cx.add(
            "internal-route",
            format!("{} ({ip}) is routed via {via}, not the VPN tunnel", i.target.host),
            remedy,
        );
        failed.push(format!("{} bypasses tunnel", i.target.host));
    }
    if !failed.is_empty() {
        return check(name, State::Failed, failed.join("; "));
    }
    let default = f.default_if.as_deref().unwrap_or("-");
    let mode = match (f.full_tunnel(), f.tunnels().is_empty()) {
        (true, _) => "; full tunnel",
        (false, false) => "; split tunnel",
        _ => "",
    };
    check(name, State::Healthy, format!("default via {default}{mode}"))
}

// Append a recent-wake hint to VPN-related findings.
fn annotate_wake(f: &Facts, findings: &mut [Finding]) {
    let Some(age) = f.wake_age.filter(|a| *a < RECENT_WAKE) else {
        return;
    };
    let vpn_codes = ["vpn-", "corp-", "stale-", "internal-route"];
    for finding in findings.iter_mut().filter(|x| vpn_codes.iter().any(|c| x.code.starts_with(c))) {
        finding
            .summary
            .push_str(&format!(" [woke from sleep {} min ago: a stale VPN session is likely]", age / 60));
    }
}

// Assess every dimension of a snapshot, given the repair steps already tried.
pub fn assess(f: &Facts, tried: &[Step]) -> Report {
    let mut cx = Ctx { f, tried, out: Vec::new() };
    let local = local(&mut cx);
    let local_ok = local.state != State::Failed;
    let mut checks = vec![local, public(&mut cx, local_ok)];
    checks.push(vpn(&mut cx));
    checks.push(routing(&mut cx));
    checks.push(corp_dns(&mut cx));
    checks.push(corp_service(&mut cx));
    checks.push(proxy(&mut cx));
    let mut findings = cx.out;
    annotate_wake(f, &mut findings);
    Report { checks, findings }
}

// Scenario tests over hand-built snapshots.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod tests {
    use super::*;
    use crate::facts::Public;
    use crate::parse::{IfStatus, Proxy, Resolver, Route, Service, Target, VpnService};

    // Build a route for tests.
    pub fn route(dest: &str, flags: &str, netif: &str) -> Route {
        Route { v6: false, dest: dest.into(), gateway: "x".into(), flags: flags.into(), netif: netif.into() }
    }

    // Build a fully healthy Wi-Fi snapshot with no VPN.
    pub fn healthy() -> Facts {
        Facts {
            phys: Some(Service { name: "Wi-Fi".into(), port: "Wi-Fi".into(), device: "en0".into() }),
            wifi_power: Some(true),
            link: IfStatus { active: true, ipv4: Some("192.168.1.5".into()), ipv6_global: false },
            dhcp: true,
            gateway: Some("192.168.1.1".into()),
            gateway_ok: Some(true),
            up_ifaces: vec!["lo0".into(), "en0".into(), "utun0".into()],
            routes: vec![route("default", "UGScg", "en0"), route("192.168.1", "UCS", "en0")],
            default_if: Some("en0".into()),
            public_if: Some("en0".into()),
            resolvers: vec![Resolver {
                nameservers: vec!["192.168.1.1".into()],
                iface: Some("en0".into()),
                reachable: Some(true),
                ..Resolver::default()
            }],
            public: Public {
                captive: false,
                ip_ok: true,
                ip6_ok: None,
                dns_ok: true,
                direct_dns_ok: None,
                https_ok: true,
            },
            ..Facts::default()
        }
    }

    // Add a connected split-tunnel scutil VPN on utun4 with split DNS for corp.example.
    pub fn with_vpn(mut f: Facts) -> Facts {
        f.up_ifaces.push("utun4".into());
        f.routes.push(route("10/8", "UCS", "utun4"));
        f.vpns =
            vec![VpnService { name: "Work".into(), state: "Connected".into(), iface: Some("utun4".into()) }];
        f.resolvers.push(Resolver {
            domain: Some("corp.example".into()),
            nameservers: vec!["10.0.0.53".into()],
            iface: Some("utun4".into()),
            reachable: Some(true),
            ..Resolver::default()
        });
        f
    }

    // Build an internal probe result for wiki.corp.example:443.
    pub fn internal(addrs: Result<Vec<&str>, &str>, route_if: &str, tcp_ok: Option<bool>) -> Internal {
        Internal {
            target: Target { host: "wiki.corp.example".into(), port: Some(443) },
            addrs: addrs.map(|a| a.iter().map(|s| s.parse().unwrap()).collect()).map_err(str::to_string),
            route_if: Some(route_if.into()),
            tcp_ok,
            resolver: None,
            direct_ok: None,
        }
    }

    // Return the state of a named check.
    fn st(r: &Report, name: &str) -> Option<State> {
        r.checks.iter().find(|c| c.name == name).map(|c| c.state)
    }

    // Return the finding codes of a report.
    fn codes(r: &Report) -> Vec<&'static str> {
        r.findings.iter().map(|f| f.code).collect()
    }

    // Check that a healthy network yields no findings and untested corporate checks.
    #[test]
    fn test_assess_healthy_no_findings() {
        let r = assess(&healthy(), &[]);
        assert!(r.healthy());
        assert!(r.findings.is_empty());
        assert_eq!(st(&r, "Corporate DNS"), Some(State::NotTested));
        assert_eq!(st(&r, "Corporate service"), Some(State::NotTested));
        assert_eq!(st(&r, "VPN connection"), Some(State::Absent));
        assert_eq!(st(&r, "Proxy"), Some(State::Absent));
    }

    // Check that healthy public internet does not hide a broken VPN service.
    #[test]
    fn test_assess_public_ok_vpn_broken() {
        let mut f = with_vpn(healthy());
        f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "utun4", Some(false))];
        let r = assess(&f, &[]);
        assert_eq!(st(&r, "Public internet"), Some(State::Healthy));
        assert!(!r.healthy());
        assert_eq!(codes(&r), vec!["corp-service-vpn"]);
        assert_eq!(r.findings[0].remedy, Remedy::Steps(vec![Step::ReconnectVpn("Work".into())]));
        assert!(!r.wifi_reset_needed());
    }

    // Check that a corporate DNS failure with a dead VPN DNS server suggests a VPN reconnect, not Wi-Fi.
    #[test]
    fn test_assess_corp_dns_server_down() {
        let mut f = with_vpn(healthy());
        let mut i = internal(Err("nodename nor servname provided"), "utun4", None);
        i.resolver = Some(f.resolvers[1].clone());
        i.direct_ok = Some(false);
        f.internal = vec![i];
        let r = assess(&f, &[]);
        assert_eq!(st(&r, "Corporate DNS"), Some(State::Failed));
        assert_eq!(st(&r, "Corporate service"), Some(State::NotTested));
        assert_eq!(codes(&r), vec!["corp-dns-server"]);
        assert!(!r.wifi_reset_needed());
    }

    // Check that a resolver fault answered directly by the server escalates from flush to reboot.
    #[test]
    fn test_assess_corp_dns_resolver_then_reboot() {
        let mut f = with_vpn(healthy());
        let mut i = internal(Err("timeout"), "utun4", None);
        i.resolver = Some(f.resolvers[1].clone());
        i.direct_ok = Some(true);
        f.internal = vec![i];
        assert_eq!(assess(&f, &[]).findings[0].remedy, Remedy::Steps(vec![Step::FlushDns]));
        assert!(matches!(assess(&f, &[Step::FlushDns]).findings[0].remedy, Remedy::Reboot(_)));
    }

    // Check that correct DNS but a route outside the tunnel is a routing failure.
    #[test]
    fn test_assess_internal_route_bypasses_tunnel() {
        let mut f = with_vpn(healthy());
        f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "en0", Some(false))];
        let r = assess(&f, &[]);
        assert_eq!(st(&r, "Routing"), Some(State::Failed));
        assert_eq!(st(&r, "Corporate DNS"), Some(State::Healthy));
        assert_eq!(codes(&r), vec!["internal-route"]);
    }

    // Check a healthy split-tunnel VPN with healthy public internet.
    #[test]
    fn test_assess_split_tunnel_healthy() {
        let mut f = with_vpn(healthy());
        f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "utun4", Some(true))];
        let r = assess(&f, &[]);
        assert!(r.healthy(), "{:?}", r.findings);
        assert!(r
            .checks
            .iter()
            .find(|c| c.name == "VPN connection")
            .unwrap()
            .detail
            .contains("split tunnel"));
    }

    // Check that a dead full-tunnel VPN is blamed instead of the Wi-Fi.
    #[test]
    fn test_assess_full_tunnel_dead() {
        let mut f = with_vpn(healthy());
        f.routes.push(route("default", "UGSc", "utun4"));
        f.public_if = Some("utun4".into());
        f.public = Public { ip_ok: false, https_ok: false, dns_ok: false, ..f.public };
        let r = assess(&f, &[]);
        assert_eq!(codes(&r), vec!["vpn-full-tunnel-dead"]);
        assert!(!r.wifi_reset_needed());
        assert_eq!(st(&r, "Local network"), Some(State::Healthy));
    }

    // Check that two VPN clients claiming the default route are reported as a conflict.
    #[test]
    fn test_assess_multiple_vpn_conflict() {
        let mut f = with_vpn(healthy());
        f.up_ifaces.push("utun6".into());
        f.routes.push(route("default", "UGSc", "utun4"));
        f.routes.push(route("default", "UGSc", "utun6"));
        let r = assess(&f, &[]);
        assert!(codes(&r).contains(&"vpn-conflict"));
        assert!(matches!(r.findings[0].remedy, Remedy::Manual(_)));
    }

    // Check that a third-party tunnel is observed as unsupported rather than healthy.
    #[test]
    fn test_assess_third_party_tunnel() {
        let mut f = healthy();
        f.up_ifaces.push("utun7".into());
        f.routes.push(route("10/8", "UCS", "utun7"));
        let r = assess(&f, &[]);
        assert_eq!(st(&r, "VPN connection"), Some(State::Unsupported));
        assert!(r.healthy());
    }

    // Check stale state after sleep: VPN interface gone, leftover routes and resolvers.
    #[test]
    fn test_assess_stale_after_wake() {
        let mut f = with_vpn(healthy());
        f.up_ifaces.retain(|i| i != "utun4");
        f.wake_age = Some(600);
        let r = assess(&f, &[]);
        let c = codes(&r);
        assert!(c.contains(&"vpn-stale") && c.contains(&"stale-routes") && c.contains(&"stale-dns"), "{c:?}");
        assert!(r.findings.iter().all(|x| x.summary.contains("woke from sleep 10 min ago")));
        assert!(!r.wifi_reset_needed());
    }

    // Check that leftover split DNS without any VPN ends in a reboot remedy.
    #[test]
    fn test_assess_orphaned_resolver_reboot() {
        let mut f = with_vpn(healthy());
        f.vpns.clear();
        f.routes.retain(|r| r.netif != "utun4");
        f.up_ifaces.retain(|i| i != "utun4");
        let r = assess(&f, &[]);
        assert_eq!(codes(&r), vec!["stale-dns"]);
        assert!(matches!(r.findings[0].remedy, Remedy::Reboot(_)));
    }

    // Check that an unreachable internal host without a VPN asks to connect the only VPN.
    #[test]
    fn test_assess_no_vpn_connected() {
        let mut f = healthy();
        f.vpns = vec![VpnService { name: "Work".into(), state: "Disconnected".into(), iface: None }];
        f.internal = vec![internal(Err("nodename nor servname provided"), "en0", None)];
        let r = assess(&f, &[]);
        assert_eq!(codes(&r), vec!["corp-no-vpn"]);
        assert_eq!(r.findings[0].remedy, Remedy::Steps(vec![Step::ReconnectVpn("Work".into())]));
    }

    // Check that a failed VPN reconnect turns into manual advice.
    #[test]
    fn test_assess_after_reconnect_failure() {
        let mut f = with_vpn(healthy());
        f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "utun4", Some(false))];
        let r = assess(&f, &[Step::ReconnectVpn("Work".into())]);
        assert!(matches!(&r.findings[0].remedy, Remedy::Manual(t) if t.contains("MFA")));
    }

    // Check the physical ladder for no address, then manual advice once exhausted.
    #[test]
    fn test_assess_no_address_ladder() {
        let mut f = healthy();
        f.link.ipv4 = None;
        let r = assess(&f, &[]);
        assert_eq!(r.findings[0].remedy, Remedy::Steps(vec![Step::RenewDhcp, Step::CycleWifi]));
        assert!(r.wifi_reset_needed());
        assert_eq!(st(&r, "Public internet"), Some(State::Healthy));
        let r = assess(&f, &[Step::RenewDhcp, Step::CycleWifi]);
        assert!(matches!(r.findings[0].remedy, Remedy::Manual(_)));
    }

    // Check that IPv6-only and dual-stack links count as addressed.
    #[test]
    fn test_assess_ipv6_only_and_dual_stack() {
        let mut f = healthy();
        f.link = IfStatus { active: true, ipv4: None, ipv6_global: true };
        f.gateway = None;
        f.gateway_ok = None;
        let r = assess(&f, &[]);
        assert_eq!(st(&r, "Local network"), Some(State::Healthy));
        let mut f = healthy();
        f.link.ipv6_global = true;
        f.public.ip6_ok = Some(false);
        let r = assess(&f, &[]);
        assert!(r.healthy());
        assert!(r.checks[1].detail.contains("IPv6 path fails"));
    }

    // Check that a powered-off radio is fixable and a radio that stays off needs a reboot.
    #[test]
    fn test_assess_wifi_off_then_reboot() {
        let mut f = healthy();
        f.wifi_power = Some(false);
        assert_eq!(assess(&f, &[]).findings[0].remedy, Remedy::Steps(vec![Step::WifiOn]));
        assert!(matches!(assess(&f, &[Step::WifiOn]).findings[0].remedy, Remedy::Reboot(_)));
    }

    // Check that hung configuration commands are reported as reboot-only.
    #[test]
    fn test_assess_config_hung_reboot() {
        let f = Facts { config_hung: true, ..healthy() };
        let r = assess(&f, &[]);
        assert_eq!(codes(&r), vec!["config-hung"]);
        assert!(matches!(r.findings[0].remedy, Remedy::Reboot(_)));
    }

    // Check the public layers: captive portal, upstream outage, DNS and HTTPS.
    #[test]
    fn test_assess_public_layers() {
        let mut f = healthy();
        f.public.captive = true;
        assert_eq!(codes(&assess(&f, &[])), vec!["captive-portal"]);
        let mut f = healthy();
        f.public = Public { ip_ok: false, https_ok: false, ..f.public };
        let r = assess(&f, &[]);
        assert_eq!(codes(&r), vec!["upstream"]);
        assert_eq!(
            r.findings[0].remedy,
            Remedy::Steps(vec![Step::ClearArp, Step::RenewDhcp, Step::CycleWifi])
        );
        let mut f = healthy();
        f.public.dns_ok = false;
        f.public.direct_dns_ok = Some(false);
        assert_eq!(assess(&f, &[]).findings[0].remedy, Remedy::Steps(vec![Step::FlushDns, Step::RenewDhcp]));
        let mut f = healthy();
        f.public.https_ok = false;
        assert!(matches!(assess(&f, &[Step::FlushDns]).findings[0].remedy, Remedy::Manual(_)));
    }

    // Check that unreachable proxies are fixable and a dead PAC file is manual.
    #[test]
    fn test_assess_proxy() {
        let mut f = healthy();
        f.proxy = Proxy {
            http: Some(("proxy.corp".into(), 8080)),
            https: None,
            pac: Some("http://wpad/x.pac".into()),
        };
        f.proxy_down = vec!["HTTP proxy proxy.corp:8080".into()];
        f.pac_ok = Some(false);
        let r = assess(&f, &[]);
        assert_eq!(codes(&r), vec!["proxy-down", "pac-down"]);
        assert_eq!(r.findings[0].remedy, Remedy::Steps(vec![Step::FixProxy]));
    }

    // Check that an internal name resolving to a sinkhole address is flagged.
    #[test]
    fn test_assess_sinkholed_name() {
        let mut f = with_vpn(healthy());
        f.internal = vec![internal(Ok(vec!["0.0.0.0"]), "lo0", Some(false))];
        assert!(codes(&assess(&f, &[])).contains(&"corp-dns-blocked"));
    }

    // Check a wired Mac with no gateway or DHCP: an upstream outage has no tool steps, only advice.
    #[test]
    fn test_assess_wired_upstream_manual() {
        let mut f = healthy();
        f.phys.as_mut().unwrap().port = "Ethernet".into();
        (f.gateway, f.gateway_ok, f.dhcp) = (None, None, false);
        f.public = Public { ip_ok: false, https_ok: false, ..f.public };
        assert!(matches!(assess(&f, &[]).findings[0].remedy, Remedy::Manual(_)));
    }

    // Check missing hardware, missing Wi-Fi and an unreachable router.
    #[test]
    fn test_assess_local_failures() {
        assert_eq!(codes(&assess(&Facts { phys: None, ..healthy() }, &[])), vec!["no-interface"]);
        assert_eq!(codes(&assess(&Facts { wifi_missing: true, ..healthy() }, &[])), vec!["wifi-missing"]);
        let mut f = healthy();
        (f.gateway_ok, f.public.ip_ok, f.public.https_ok) = (Some(false), false, false);
        let r = assess(&f, &[]);
        assert_eq!(codes(&r), vec!["gateway-unreachable"]);
        assert_eq!(r.checks[1].detail, "unreachable: local network failed");
        assert_eq!(
            r.findings[0].remedy,
            Remedy::Steps(vec![Step::ClearArp, Step::RenewDhcp, Step::CycleWifi])
        );
    }

    // Check a healthy full tunnel, and a blocked raw-IP probe while HTTPS works.
    #[test]
    fn test_assess_public_details() {
        let mut f = with_vpn(healthy());
        f.public_if = Some("utun4".into());
        assert!(assess(&f, &[]).checks[1].detail.ends_with("via tunnel utun4"));
        let mut f = healthy();
        f.public.ip_ok = false;
        let r = assess(&f, &[]);
        assert!(r.healthy());
        assert!(r.checks[1].detail.contains("raw-IP probe"));
    }

    // Check public DNS failures: resolver at fault, full-tunnel VPN DNS, and no DHCP to renew.
    #[test]
    fn test_assess_public_dns_variants() {
        let mut f = healthy();
        (f.public.dns_ok, f.public.direct_dns_ok) = (false, Some(true));
        assert_eq!(codes(&assess(&f, &[])), vec!["dns-resolver"]);
        let mut f = with_vpn(healthy());
        (f.public.dns_ok, f.public_if) = (false, Some("utun4".into()));
        let want = Remedy::Steps(vec![Step::FlushDns, Step::ReconnectVpn("Work".into())]);
        assert_eq!(assess(&f, &[]).findings[0].remedy, want);
        let mut f = healthy();
        (f.public.dns_ok, f.dhcp) = (false, false);
        assert_eq!(assess(&f, &[]).findings[0].remedy, Remedy::Steps(vec![Step::FlushDns]));
    }

    // Check VPN states: stuck connecting, extra tunnels, and an unrecognized state.
    #[test]
    fn test_assess_vpn_states() {
        let mut f = with_vpn(healthy());
        f.vpns[0].state = "Connecting".into();
        assert_eq!(codes(&assess(&f, &[])), vec!["vpn-stuck"]);
        let mut f = with_vpn(healthy());
        f.routes.push(route("172.16/12", "UCS", "utun7"));
        f.up_ifaces.push("utun7".into());
        assert!(assess(&f, &[]).checks[2].detail.ends_with("other tunnels: utun7"));
        let mut f = healthy();
        f.vpns = vec![VpnService { name: "Odd".into(), state: "Invalid".into(), iface: None }];
        assert_eq!(st(&assess(&f, &[]), "VPN connection"), Some(State::Unknown));
    }

    // Check routing: a public internal address via Wi-Fi, a missing default route, stale routes without a VPN.
    #[test]
    fn test_assess_routing_variants() {
        let mut f = with_vpn(healthy());
        f.internal = vec![internal(Ok(vec!["93.184.216.34"]), "en0", Some(true))];
        assert!(assess(&f, &[]).healthy());
        let mut f = healthy();
        f.default_if = None;
        assert_eq!(assess(&f, &[]).findings[0].remedy, Remedy::Steps(vec![Step::RenewDhcp]));
        f.dhcp = false;
        assert!(matches!(assess(&f, &[]).findings[0].remedy, Remedy::Manual(_)));
        let mut f = healthy();
        f.routes.push(route("10/8", "UCS", "utun9"));
        let r = assess(&f, &[]);
        assert_eq!(r.findings[0].remedy, Remedy::Steps(vec![Step::FlushRoutes]));
        assert!(matches!(assess(&f, &[Step::FlushRoutes]).findings[0].remedy, Remedy::Reboot(_)));
    }

    // Check that a third-party tunnel bypass gets manual advice, as no scutil VPN can be reconnected.
    #[test]
    fn test_assess_vpn_fix_manual_without_scutil() {
        let mut f = healthy();
        f.up_ifaces.push("utun7".into());
        f.routes.push(route("10/8", "UCS", "utun7"));
        f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "en0", Some(false))];
        let r = assess(&f, &[]);
        assert_eq!(codes(&r), vec!["internal-route"]);
        assert!(matches!(r.findings[0].remedy, Remedy::Manual(_)));
    }

    // Check the remaining branches: a router that ignores ping, split-tunnel DNS, no address and no route.
    #[test]
    fn test_assess_quiet_router_split_dns_no_route() {
        let mut f = healthy();
        f.gateway_ok = Some(false);
        assert!(assess(&f, &[]).healthy());
        let mut f = with_vpn(healthy());
        f.public.dns_ok = false;
        assert_eq!(assess(&f, &[]).findings[0].remedy, Remedy::Steps(vec![Step::FlushDns, Step::RenewDhcp]));
        let mut f = healthy();
        (f.default_if, f.link.ipv4) = (None, None);
        assert_eq!(codes(&assess(&f, &[])), vec!["no-address"]);
    }
}
