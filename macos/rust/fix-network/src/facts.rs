// Gather a fresh snapshot of link, routing, DNS, proxy and VPN state.
use std::net::IpAddr;

use crate::parse::{
    is_tunnel, parse_dig, parse_dns, parse_ifconfig, parse_proxy, parse_route_if, parse_routes,
    parse_services, parse_vpn_list, parse_vpn_status, parse_waketime, IfStatus, Proxy, Resolver, Route,
    Service, Target, VpnService,
};
use crate::sys::Sys;

const PUBLIC_V4: &str = "1.1.1.1";
const PUBLIC_V6: &str = "2606:4700:4700::1111";

// Results of the public-internet probes.
#[derive(Clone, Debug, Default)]
pub struct Public {
    pub captive: bool,
    pub ip_ok: bool,
    pub ip6_ok: Option<bool>,
    pub dns_ok: bool,
    pub direct_dns_ok: Option<bool>,
    pub https_ok: bool,
}

// Results of probing one internal host.
#[derive(Clone, Debug)]
pub struct Internal {
    pub target: Target,
    pub addrs: Result<Vec<IpAddr>, String>,
    pub route_if: Option<String>,
    pub tcp_ok: Option<bool>,
    pub resolver: Option<Resolver>,
    pub direct_ok: Option<bool>,
}

// Everything one diagnosis pass observed.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub macos: String,
    pub over_ssh: bool,
    pub config_hung: bool,
    pub wake_age: Option<u64>,
    pub phys: Option<Service>,
    pub wifi_power: Option<bool>,
    pub wifi_missing: bool,
    pub link: IfStatus,
    pub dhcp: bool,
    pub gateway: Option<String>,
    pub gateway_ok: Option<bool>,
    pub up_ifaces: Vec<String>,
    pub routes: Vec<Route>,
    pub default_if: Option<String>,
    pub public_if: Option<String>,
    pub resolvers: Vec<Resolver>,
    pub proxy: Proxy,
    pub proxy_down: Vec<String>,
    pub pac_ok: Option<bool>,
    pub vpns: Vec<VpnService>,
    pub public: Public,
    pub internal: Vec<Internal>,
}

impl Facts {
    // Return tunnel interfaces that carry real (non link-local) routes, in table order.
    pub fn tunnels(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in self.routes.iter().filter(|r| is_tunnel(&r.netif) && !r.link_local()) {
            if !(r.dest == "default" && r.scoped()) && !out.contains(&r.netif) {
                out.push(r.netif.clone());
            }
        }
        out
    }

    // Return tunnel interfaces that own an unscoped default route.
    pub fn tunnel_defaults(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in self.routes.iter().filter(|r| r.dest == "default" && !r.scoped() && is_tunnel(&r.netif)) {
            if !out.contains(&r.netif) {
                out.push(r.netif.clone());
            }
        }
        out
    }

    // Tell whether public traffic is routed into a tunnel.
    pub fn full_tunnel(&self) -> bool {
        self.public_if.as_deref().is_some_and(is_tunnel)
    }

    // Return the first scutil-managed VPN that reports Connected.
    pub fn connected_vpn(&self) -> Option<&VpnService> {
        self.vpns.iter().find(|v| v.state == "Connected")
    }

    // Return routes whose interface is no longer up.
    pub fn stale_routes(&self) -> Vec<&Route> {
        self.routes
            .iter()
            .filter(|r| !r.link_local() && !r.netif.is_empty() && !self.up_ifaces.contains(&r.netif))
            .collect()
    }

    // Return split-DNS resolvers whose interface is gone or whose servers are unreachable.
    pub fn stale_resolvers(&self) -> Vec<&Resolver> {
        self.resolvers
            .iter()
            .filter(|r| r.supplemental())
            .filter(|r| {
                r.reachable == Some(false) || r.iface.as_ref().is_some_and(|i| !self.up_ifaces.contains(i))
            })
            .collect()
    }

    // Tell whether any VPN is connected or any tunnel carries routes.
    pub fn vpn_present(&self) -> bool {
        self.connected_vpn().is_some() || !self.tunnels().is_empty()
    }
}

// Return the most specific split-DNS resolver for a host name.
pub fn match_resolver<'a>(resolvers: &'a [Resolver], host: &str) -> Option<&'a Resolver> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    resolvers
        .iter()
        .filter(|r| r.supplemental())
        .filter(|r| {
            let d = r.domain.as_deref().unwrap_or_default().to_ascii_lowercase();
            host == d || host.ends_with(&format!(".{d}"))
        })
        .max_by_key(|r| r.domain.as_ref().map_or(0, String::len))
}

// Pick the physical service in use: the default-route owner, else the first one with an address.
pub fn choose_phys(candidates: &[(Service, IfStatus)], default_if: Option<&str>) -> Option<usize> {
    let has_addr = |s: &IfStatus| s.active && (s.ipv4.is_some() || s.ipv6_global);
    candidates
        .iter()
        .position(|(svc, _)| Some(svc.device.as_str()) == default_if)
        .or_else(|| candidates.iter().position(|(_, st)| has_addr(st)))
        .or_else(|| candidates.iter().position(|(svc, _)| svc.is_wifi()))
        .or(if candidates.is_empty() { None } else { Some(0) })
}

// Return a command's output, ignoring its exit code.
fn out(sys: &dyn Sys, cmd: &[&str]) -> String {
    sys.run(cmd, 15, false).1
}

// Tell whether a TCP connection to a host name succeeds within the timeout.
pub fn tcp_host(sys: &dyn Sys, host: &str, port: u16, timeout: u64) -> bool {
    sys.resolve(host, timeout).is_ok_and(|a| sys.tcp(&a, port, timeout))
}

// Ask a DNS server directly; None when dig is unavailable.
fn direct_lookup(sys: &dyn Sys, server: &str, name: &str) -> Option<bool> {
    let at = format!("@{server}");
    let (rc, text) = sys.run(&["dig", "+time=3", "+tries=1", "+short", &at, name], 10, false);
    if rc == 127 {
        return None;
    }
    Some(rc == 0 && !parse_dig(&text).is_empty())
}

// Return the first nameserver of the primary (unscoped, domain-less) resolver.
pub fn primary_nameserver(resolvers: &[Resolver]) -> Option<&str> {
    resolvers
        .iter()
        .find(|r| !r.scoped && r.domain.is_none() && !r.nameservers.is_empty())
        .map(|r| r.nameservers[0].as_str())
}

// Return the interface a route lookup picks for an address.
fn route_if(sys: &dyn Sys, ip: &IpAddr) -> Option<String> {
    let text = ip.to_string();
    let mut cmd = vec!["route", "-n", "get"];
    if ip.is_ipv6() {
        cmd.push("-inet6");
    }
    cmd.push(&text);
    parse_route_if(&out(sys, &cmd))
}

// Return seconds since the last wake from sleep, if the Mac has slept.
fn wake_age(sys: &dyn Sys) -> Option<u64> {
    let woke = parse_waketime(&out(sys, &["sysctl", "-n", "kern.waketime"]))?;
    Some(sys.now().saturating_sub(woke))
}

// Probe the public internet in layers: captive portal, IP path, DNS and HTTPS.
fn public_probes(sys: &dyn Sys, f: &Facts) -> Public {
    let v4: IpAddr = PUBLIC_V4.parse().expect("valid literal");
    let v6: IpAddr = PUBLIC_V6.parse().expect("valid literal");
    let captive_url = "http://captive.apple.com/hotspot-detect.html";
    let (rc, body) = sys.run(&["curl", "-sS", "--max-time", "8", captive_url], 12, false);
    let https = ["curl", "-sS", "-o", "/dev/null", "--max-time", "8", "https://www.apple.com/"];
    let ip6 = f.link.ipv6_global.then(|| sys.tcp(&[v6], 443, 4));
    let has_v4 = f.link.ipv4.is_some();
    let ip_ok = if has_v4 { sys.tcp(&[v4], 443, 4) } else { ip6 == Some(true) };
    let dns_ok = sys.resolve("www.apple.com", 6).is_ok();
    let direct_dns_ok = match (dns_ok, primary_nameserver(&f.resolvers)) {
        (false, Some(ns)) => direct_lookup(sys, ns, "www.apple.com"),
        _ => None,
    };
    Public {
        captive: rc == 0 && !body.contains("Success"),
        ip_ok,
        ip6_ok: if has_v4 { ip6 } else { None },
        dns_ok,
        direct_dns_ok,
        https_ok: sys.run(&https, 12, false).0 == 0,
    }
}

// Probe an internal host: name, the resolver that should answer, route and TCP.
fn probe_internal(sys: &dyn Sys, t: &Target, resolvers: &[Resolver]) -> Internal {
    let addrs = sys.resolve(&t.host, 6);
    let resolver = match_resolver(resolvers, &t.host).cloned();
    let server = match &resolver {
        Some(r) => Some(r.nameservers[0].as_str()),
        None => primary_nameserver(resolvers),
    };
    let direct_ok = match (&addrs, server) {
        (Err(_), Some(ns)) => direct_lookup(sys, ns, &t.host),
        _ => None,
    };
    let first = addrs.as_ref().ok().and_then(|a| a.first().copied());
    let tcp_ok = match (&addrs, t.port) {
        (Ok(a), Some(p)) => Some(sys.tcp(a, p, 6)),
        _ => None,
    };
    let route_if = first.and_then(|ip| route_if(sys, &ip));
    Internal { target: t.clone(), route_if, addrs, tcp_ok, resolver, direct_ok }
}

// Record the link, gateway and Wi-Fi facts of the chosen physical service.
fn link_facts(sys: &dyn Sys, f: &mut Facts, all_ifaces: &[String]) {
    let Some(p) = f.phys.clone() else {
        return;
    };
    f.link = parse_ifconfig(&out(sys, &["ifconfig", &p.device]));
    if p.is_wifi() {
        let power = out(sys, &["networksetup", "-getairportpower", &p.device]);
        f.wifi_power = Some(power.trim_end().ends_with("On"));
        f.wifi_missing = !all_ifaces.contains(&p.device);
    }
    f.dhcp = out(sys, &["networksetup", "-getinfo", &p.name]).contains("DHCP Configuration");
    let gw = out(sys, &["ipconfig", "getoption", &p.device, "router"]);
    if gw.parse::<std::net::Ipv4Addr>().is_ok() {
        f.gateway_ok = Some(sys.run(&["ping", "-c", "2", "-t", "4", &gw], 10, false).0 == 0);
        f.gateway = Some(gw);
    }
}

// Record which enabled proxies and PAC file are unreachable.
fn proxy_facts(sys: &dyn Sys, f: &mut Facts) {
    f.proxy = parse_proxy(&out(sys, &["scutil", "--proxy"]));
    for (kind, server) in [("HTTP", &f.proxy.http), ("HTTPS", &f.proxy.https)] {
        let Some((host, port)) = server else {
            continue;
        };
        if !tcp_host(sys, host, *port, 3) {
            f.proxy_down.push(format!("{kind} proxy {host}:{port}"));
        }
    }
    if let Some(url) = &f.proxy.pac {
        let rc = sys.run(&["curl", "-sS", "-o", "/dev/null", "--max-time", "5", url], 8, false).0;
        f.pac_ok = Some(rc == 0);
    }
}

// Record scutil-managed VPN services and the state and tunnel interface of active ones.
fn vpn_facts(sys: &dyn Sys, f: &mut Facts) {
    f.vpns = parse_vpn_list(&out(sys, &["scutil", "--nc", "list"]));
    for v in f.vpns.iter_mut().filter(|v| v.state != "Disconnected") {
        let (state, iface) = parse_vpn_status(&out(sys, &["scutil", "--nc", "status", &v.name]));
        if !state.is_empty() {
            v.state = state;
        }
        v.iface = iface;
    }
}

// Split command output into words.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

// Take a full, fresh snapshot of the network state.
pub fn gather(sys: &dyn Sys, targets: &[Target]) -> Facts {
    let (rc_order, order) = sys.run(&["networksetup", "-listnetworkserviceorder"], 15, false);
    let (rc_dns, dns) = sys.run(&["scutil", "--dns"], 15, false);
    let all_ifaces = words(&out(sys, &["ifconfig", "-l"]));
    let default_if = parse_route_if(&out(sys, &["route", "-n", "get", "default"]))
        .or_else(|| parse_route_if(&out(sys, &["route", "-n", "get", "-inet6", "default"])));
    let mut f = Facts {
        macos: out(sys, &["sw_vers", "-productVersion"]),
        over_ssh: sys.over_ssh(),
        config_hung: rc_order == 124 || rc_dns == 124,
        wake_age: wake_age(sys),
        up_ifaces: words(&out(sys, &["ifconfig", "-l", "-u"])),
        routes: parse_routes(&out(sys, &["netstat", "-rn"])),
        default_if,
        public_if: parse_route_if(&out(sys, &["route", "-n", "get", PUBLIC_V4])),
        resolvers: parse_dns(&dns),
        ..Facts::default()
    };
    let candidates: Vec<(Service, IfStatus)> = parse_services(&order)
        .into_iter()
        .map(|s| {
            let st = parse_ifconfig(&out(sys, &["ifconfig", &s.device]));
            (s, st)
        })
        .collect();
    f.phys = choose_phys(&candidates, f.default_if.as_deref()).map(|i| candidates[i].0.clone());
    link_facts(sys, &mut f, &all_ifaces);
    proxy_facts(sys, &mut f);
    vpn_facts(sys, &mut f);
    f.public = public_probes(sys, &f);
    f.internal = targets.iter().map(|t| probe_internal(sys, t, &f.resolvers)).collect();
    f
}

// Tests for snapshot helpers and gathering against a scripted Mac.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod tests {
    use super::*;
    use crate::sys::fake::Fake;

    pub const ORDER: &str = "An asterisk (*) denotes that a network service is disabled.\n(1) Wi-Fi\n\
        (Hardware Port: Wi-Fi, Device: en0)\n\n(2) Work\n(Hardware Port: VPN, Device: )\n";
    pub const DNS: &str =
        "DNS configuration\n\nresolver #1\n  nameserver[0] : 192.168.1.1\n  if_index : 6 (en0)\n  \
        reach    : 0x00020002 (Reachable)\n";
    pub const SPLIT: &str = "\nresolver #2\n  domain   : corp.example\n  nameserver[0] : 10.0.0.53\n  \
        if_index : 22 (utun4)\n  reach    : 0x00000002 (Reachable)\n";
    pub const ROUTES: &str =
        "Internet:\nDestination Gateway Flags Netif Expire\ndefault 192.168.1.1 UGScg en0\n";
    pub const IFCONFIG: &str =
        "en0: flags=8863<UP>\n\tinet 192.168.1.5 netmask 0xffffff00\n\tstatus: active\n";
    pub const CAPTIVE: &str = "curl -sS --max-time 8 http://captive.apple.com/hotspot-detect.html";
    pub const HTTPS: &str = "curl -sS -o /dev/null --max-time 8 https://www.apple.com/";

    // Script a healthy Wi-Fi Mac with no VPN, awake for 2 hours.
    pub fn mac() -> Fake {
        let f = Fake { clock: 1_000_000, ..Fake::default() };
        f.on("networksetup -listnetworkserviceorder", &[(0, ORDER)])
            .on("scutil --dns", &[(0, DNS)])
            .on("ifconfig -l", &[(0, "lo0 en0 utun0")])
            .on("ifconfig -l -u", &[(0, "lo0 en0 utun0")])
            .on("sw_vers -productVersion", &[(0, "15.1")])
            .on("sysctl -n kern.waketime", &[(0, "{ sec = 992800, usec = 0 }")])
            .on("netstat -rn", &[(0, ROUTES)])
            .on("route -n get default", &[(0, "  interface: en0")])
            .on("route -n get 1.1.1.1", &[(0, "  interface: en0")])
            .on("ifconfig en0", &[(0, IFCONFIG)])
            .on("networksetup -getairportpower en0", &[(0, "Wi-Fi Power (en0): On")])
            .on("networksetup -getinfo Wi-Fi", &[(0, "DHCP Configuration\nIP address: 192.168.1.5")])
            .on("ipconfig getoption en0 router", &[(0, "192.168.1.1")])
            .on("ping -c 2 -t 4 192.168.1.1", &[(0, "")])
            .on("scutil --proxy", &[(0, "<dictionary> {\n}")])
            .on("scutil --nc list", &[(0, "Available network connection services:\n")])
            .on(CAPTIVE, &[(0, "<BODY>Success</BODY>")])
            .on(HTTPS, &[(0, "")])
            .name("www.apple.com", &["17.253.144.10"])
            .listen("1.1.1.1", 443);
        f
    }

    // Add a connected split-tunnel scutil VPN "Work" on utun4 with split DNS for corp.example.
    pub fn with_vpn(f: Fake) -> Fake {
        f.on("scutil --dns", &[(0, &format!("{DNS}{SPLIT}"))])
            .on("ifconfig -l -u", &[(0, "lo0 en0 utun0 utun4")])
            .on("ifconfig -l", &[(0, "lo0 en0 utun0 utun4")])
            .on("netstat -rn", &[(0, &format!("{ROUTES}10/8 link#22 UCS utun4\n"))])
            .on(
                "scutil --nc list",
                &[(0, "* (Connected)  0A1B VPN (com.example) \"Work\"  [VPN/com.example]\n")],
            )
            .on("scutil --nc status Work", &[(0, "Connected\n  InterfaceName : utun4\n")]);
        f
    }

    // Build a route for tests.
    fn route(dest: &str, flags: &str, netif: &str) -> Route {
        Route { v6: false, dest: dest.into(), gateway: "x".into(), flags: flags.into(), netif: netif.into() }
    }

    // Build a supplemental resolver for tests.
    fn split(domain: &str, iface: &str) -> Resolver {
        Resolver {
            domain: Some(domain.into()),
            nameservers: vec!["10.0.0.53".into()],
            iface: Some(iface.into()),
            reachable: Some(true),
            ..Resolver::default()
        }
    }

    // Check that a healthy Mac gathers into the expected snapshot.
    #[test]
    fn test_gather_healthy_mac() {
        let f = gather(&mac(), &[]);
        assert_eq!(f.macos, "15.1");
        assert_eq!(f.phys.as_ref().map(|p| p.device.as_str()), Some("en0"));
        assert_eq!(f.link.ipv4.as_deref(), Some("192.168.1.5"));
        assert_eq!((f.wifi_power, f.wifi_missing, f.dhcp), (Some(true), false, true));
        assert_eq!((f.gateway.as_deref(), f.gateway_ok), (Some("192.168.1.1"), Some(true)));
        assert_eq!(f.wake_age, Some(7200));
        assert!(!f.config_hung && !f.over_ssh);
        let p = &f.public;
        assert!(!p.captive && p.ip_ok && p.dns_ok && p.https_ok);
        assert_eq!((p.ip6_ok, p.direct_dns_ok), (None, None));
        assert!(f.vpns.is_empty() && f.internal.is_empty() && f.proxy_down.is_empty());
    }

    // Check a VPN with split DNS and internal hosts that resolve, fail, or are IP literals.
    #[test]
    fn test_gather_vpn_and_internal_hosts() {
        let sys = with_vpn(mac());
        sys.name("wiki.corp.example", &["10.1.2.3"]).listen("10.1.2.3", 443);
        sys.on("route -n get 10.1.2.3", &[(0, "  interface: utun4")]);
        sys.on("dig +time=3 +tries=1 +short @10.0.0.53 down.corp.example", &[(9, ";; timed out")]);
        sys.on("dig +time=3 +tries=1 +short @192.168.1.1 other.example", &[(0, "93.184.216.34")]);
        sys.on("route -n get -inet6 fd00::7", &[(0, "  interface: utun4")]);
        let targets = ["wiki.corp.example:443", "down.corp.example:22", "other.example", "[fd00::7]:22"]
            .map(|t| crate::parse::parse_target(t).unwrap());
        let f = gather(&sys, &targets);
        assert_eq!(f.vpns[0].iface.as_deref(), Some("utun4"));
        assert_eq!(f.tunnels(), vec!["utun4"]);
        let [wiki, down, other, v6] = [&f.internal[0], &f.internal[1], &f.internal[2], &f.internal[3]];
        assert_eq!((wiki.tcp_ok, wiki.route_if.as_deref()), (Some(true), Some("utun4")));
        assert_eq!(wiki.resolver.as_ref().and_then(|r| r.domain.as_deref()), Some("corp.example"));
        assert_eq!((down.direct_ok, down.tcp_ok, down.route_if.as_deref()), (Some(false), None, None));
        assert_eq!((other.direct_ok, other.resolver.is_none()), (Some(true), true));
        assert_eq!((v6.tcp_ok, v6.route_if.as_deref()), (Some(false), Some("utun4")));
    }

    // Check a broken Mac: hung configd, radio off, no gateway, captive portal, DNS down, dig missing.
    #[test]
    fn test_gather_broken_mac() {
        let sys = mac();
        sys.on("scutil --dns", &[(124, "(timed out after 15s)")])
            .on("networksetup -getairportpower en0", &[(0, "Wi-Fi Power (en0): Off")])
            .on("ipconfig getoption en0 router", &[(0, "")])
            .on("sysctl -n kern.waketime", &[(0, "{ sec = 0, usec = 0 }")])
            .on("route -n get default", &[(1, "route: writing to routing socket: not in table")])
            .on("route -n get -inet6 default", &[(0, "  interface: en0")])
            .on("ifconfig -l", &[(0, "lo0")])
            .on(CAPTIVE, &[(0, "<html>Sign in to Hotel Wi-Fi</html>")])
            .on(HTTPS, &[(28, "timeout")]);
        sys.names.borrow_mut().clear();
        let f = gather(&Fake { ssh: true, ..sys }, &[]);
        assert!(f.config_hung && f.over_ssh && f.wifi_missing);
        assert_eq!((f.wifi_power, f.gateway, f.gateway_ok, f.wake_age), (Some(false), None, None, None));
        assert_eq!(f.default_if.as_deref(), Some("en0"));
        let p = &f.public;
        assert!(p.captive && !p.dns_ok && !p.https_ok);
        assert_eq!(p.direct_dns_ok, None);
    }

    // Check DNS falling back to a direct query, IPv6-only and dual-stack probes, and gateway ping failure.
    #[test]
    fn test_gather_dns_direct_and_ipv6() {
        let sys = mac();
        sys.names.borrow_mut().clear();
        sys.on("dig +time=3 +tries=1 +short @192.168.1.1 www.apple.com", &[(0, "17.253.144.10")])
            .on("ping -c 2 -t 4 192.168.1.1", &[(2, "100% packet loss")])
            .on("ifconfig en0", &[(0, "en0:\n\tinet6 2001:db8::5 prefixlen 64\n\tstatus: active\n")])
            .listen("2606:4700:4700::1111", 443);
        let f = gather(&sys, &[]);
        assert_eq!(f.public.direct_dns_ok, Some(true));
        assert_eq!(f.gateway_ok, Some(false));
        assert!(f.public.ip_ok && f.public.ip6_ok.is_none());
        sys.on("ifconfig en0", &[(0, &format!("{IFCONFIG}\tinet6 2001:db8::5 prefixlen 64\n"))]);
        sys.open.borrow_mut().retain(|(ip, _)| ip.is_ipv4());
        assert_eq!(gather(&sys, &[]).public.ip6_ok, Some(false));
    }

    // Check proxy reachability, PAC fetches and VPN status parsing in a gather.
    #[test]
    fn test_gather_proxy_and_vpn_states() {
        let sys = mac();
        let proxy = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 8080\n  HTTPProxy : proxy.corp\n  HTTPSEnable : 1\n  \
                     HTTPSPort : 8443\n  HTTPSProxy : up.corp\n  ProxyAutoConfigEnable : 1\n  \
                     ProxyAutoConfigURLString : http://wpad/wpad.dat\n}";
        sys.on("scutil --proxy", &[(0, proxy)])
            .on("curl -sS -o /dev/null --max-time 5 http://wpad/wpad.dat", &[(6, "could not resolve")])
            .on(
                "scutil --nc list",
                &[(0, "* (Connecting) 0A VPN (x) \"A\" [VPN/x]\n* (Disconnected) 0B IPSec \"B\" [IPSec]\n")],
            )
            .on("scutil --nc status A", &[(0, "")])
            .name("up.corp", &["10.9.9.9"])
            .listen("10.9.9.9", 8443);
        let f = gather(&sys, &[]);
        assert_eq!(f.proxy_down, vec!["HTTP proxy proxy.corp:8080"]);
        assert_eq!(f.pac_ok, Some(false));
        assert_eq!((f.vpns[0].state.as_str(), f.vpns[1].state.as_str()), ("Connecting", "Disconnected"));
        assert_eq!(sys.count("scutil --nc status B"), 0);
    }

    // Check that a wired Mac without Wi-Fi gathers without Wi-Fi facts.
    #[test]
    fn test_gather_wired_without_wifi() {
        let sys = mac();
        let order = "(1) USB LAN\n(Hardware Port: USB 10/100/1000 LAN, Device: en7)\n";
        sys.on("networksetup -listnetworkserviceorder", &[(0, order)])
            .on("route -n get default", &[(0, "  interface: en7")])
            .on("ifconfig en7", &[(0, IFCONFIG)])
            .on("networksetup -getinfo USB LAN", &[(0, "Manual Configuration")])
            .on("ipconfig getoption en7 router", &[(0, "10.0.0.1")])
            .on("ping -c 2 -t 4 10.0.0.1", &[(0, "")]);
        let f = gather(&sys, &[]);
        assert_eq!(f.phys.as_ref().map(|p| p.device.as_str()), Some("en7"));
        assert_eq!((f.wifi_power, f.dhcp), (None, false));
        sys.on("networksetup -listnetworkserviceorder", &[(0, "")]);
        assert!(gather(&sys, &[]).phys.is_none());
    }

    // Check that only tunnels with real routes count, and scoped defaults do not.
    #[test]
    fn test_tunnels_ignore_system_utuns() {
        let f = Facts {
            routes: vec![
                route("default", "UGcIg", "utun0"),
                route("10/8", "UCS", "utun4"),
                route("10.2/16", "UCS", "utun4"),
                route("default", "UGSc", "en0"),
                route("default", "UGSc", "utun6"),
                route("default", "UGSc", "utun6"),
            ],
            ..Facts::default()
        };
        assert_eq!(f.tunnels(), vec!["utun4", "utun6"]);
        assert_eq!(f.tunnel_defaults(), vec!["utun6"]);
    }

    // Check that the longest split-DNS domain wins.
    #[test]
    fn test_match_resolver_longest_suffix() {
        let rs = vec![split("corp.example", "utun4"), split("eu.corp.example", "utun4")];
        let hit = match_resolver(&rs, "wiki.eu.corp.example").unwrap();
        assert_eq!(hit.domain.as_deref(), Some("eu.corp.example"));
        assert!(match_resolver(&rs, "corp.example.").is_some());
        assert!(match_resolver(&rs, "example.org").is_none());
        assert!(match_resolver(&rs, "notcorp.example").is_none());
    }

    // Check that resolvers and routes on vanished interfaces or unreachable servers are stale.
    #[test]
    fn test_stale_state_after_tunnel_gone() {
        let mut gone = split("corp.example", "utun4");
        gone.reachable = Some(true);
        let mut unreachable = split("lab.example", "en0");
        unreachable.reachable = Some(false);
        let f = Facts {
            up_ifaces: vec!["en0".into(), "lo0".into()],
            routes: vec![
                route("10/8", "UCS", "utun4"),
                route("default", "UGSc", "en0"),
                route("224.0.0/4", "UmCS", "utun4"),
            ],
            resolvers: vec![gone, unreachable, split("ok.example", "en0"), Resolver::default()],
            ..Facts::default()
        };
        assert_eq!(f.stale_routes().len(), 1);
        assert_eq!(f.stale_resolvers().len(), 2);
    }

    // Check the physical-service choice for Ethernet, Wi-Fi and empty cases.
    #[test]
    fn test_choose_phys() {
        let wifi = Service { name: "Wi-Fi".into(), port: "Wi-Fi".into(), device: "en0".into() };
        let lan = Service { name: "LAN".into(), port: "Ethernet".into(), device: "en7".into() };
        let up = IfStatus { active: true, ipv4: Some("10.0.0.2".into()), ipv6_global: false };
        let v6 = IfStatus { active: true, ipv4: None, ipv6_global: true };
        let down = IfStatus::default();
        let c = vec![(wifi.clone(), down.clone()), (lan.clone(), up.clone())];
        assert_eq!(choose_phys(&c, Some("utun4")), Some(1));
        assert_eq!(choose_phys(&c, Some("en0")), Some(0));
        assert_eq!(choose_phys(&[(wifi.clone(), down.clone()), (lan.clone(), v6)], None), Some(1));
        assert_eq!(choose_phys(&[(lan.clone(), down.clone()), (wifi, down.clone())], None), Some(1));
        assert_eq!(choose_phys(&[(lan, down)], None), Some(0));
        assert_eq!(choose_phys(&[], None), None);
    }

    // Check the VPN-presence and full-tunnel helpers.
    #[test]
    fn test_vpn_present_and_full_tunnel() {
        let mut f = Facts::default();
        assert!(!f.vpn_present() && !f.full_tunnel());
        f.public_if = Some("utun4".into());
        assert!(f.full_tunnel());
        f.vpns = vec![VpnService { name: "W".into(), state: "Connected".into(), iface: None }];
        assert!(f.vpn_present());
        assert_eq!(primary_nameserver(&[]), None);
    }

    // Check a hung service list, no captive check, missing dig, and the primary resolver choice.
    #[test]
    fn test_gather_odd_answers() {
        let sys = mac();
        sys.on("networksetup -listnetworkserviceorder", &[(124, "(timed out after 15s)")])
            .on(CAPTIVE, &[(6, "could not resolve host")]);
        let f = gather(&sys, &[]);
        assert!(f.config_hung && !f.public.captive && f.phys.is_none());
        sys.names.borrow_mut().clear();
        let f = gather(&sys, &[]);
        assert_eq!(f.public.direct_dns_ok, None);
        let scoped = Resolver { scoped: true, nameservers: vec!["9.9.9.9".into()], ..Resolver::default() };
        let split = split("corp.example", "utun4");
        let empty = Resolver::default();
        let main = Resolver { nameservers: vec!["1.1.1.1".into()], ..Resolver::default() };
        assert_eq!(primary_nameserver(&[scoped, split, empty, main]), Some("1.1.1.1"));
    }

    // Check that a route with no interface name is never stale.
    #[test]
    fn test_stale_routes_ignore_unnamed() {
        let f = Facts { routes: vec![route("10/8", "UCS", "")], ..Facts::default() };
        assert!(f.stale_routes().is_empty());
    }
}
