// Gather a fresh snapshot of link, routing, DNS, proxy and VPN state.
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::parse::{
    is_tunnel, parse_dig, parse_dns, parse_ifconfig, parse_proxy, parse_route_if, parse_routes,
    parse_services, parse_vpn_list, parse_vpn_status, parse_waketime, IfStatus, Proxy, Resolver, Route,
    Service, Target, VpnService,
};
use crate::run::{sh, sh_opts};

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

// Resolve a name to all its addresses through the system resolver with a time limit.
pub fn resolve(name: &str, timeout: u64) -> Result<Vec<IpAddr>, String> {
    if let Ok(ip) = name.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let (tx, rx) = mpsc::channel();
    let host = name.to_string();
    thread::spawn(move || {
        let result = (host.as_str(), 0).to_socket_addrs().map_err(|e| e.to_string()).map(|addrs| {
            let mut ips: Vec<IpAddr> = Vec::new();
            for ip in addrs.map(|a| a.ip()) {
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
            ips
        });
        let _ = tx.send(result);
    });
    match rx.recv_timeout(Duration::from_secs(timeout)) {
        Ok(Ok(ips)) if ips.is_empty() => Err("no address".to_string()),
        Ok(result) => result,
        Err(_) => Err("timeout".to_string()),
    }
}

// Tell whether a TCP connection to any of the addresses succeeds within the timeout.
pub fn tcp_any(addrs: &[IpAddr], port: u16, timeout: u64) -> bool {
    let limit = Duration::from_secs(timeout);
    addrs.iter().any(|ip| TcpStream::connect_timeout(&SocketAddr::new(*ip, port), limit).is_ok())
}

// Tell whether a TCP connection to a host name succeeds within the timeout.
pub fn tcp(host: &str, port: u16, timeout: u64) -> bool {
    resolve(host, timeout).is_ok_and(|a| tcp_any(&a, port, timeout))
}

// Ask a DNS server directly; None when dig is unavailable.
fn direct_lookup(server: &str, name: &str) -> Option<bool> {
    let at = format!("@{server}");
    let (rc, out) = sh_opts(&["dig", "+time=3", "+tries=1", "+short", &at, name], 10, false);
    if rc == 127 {
        return None;
    }
    Some(rc == 0 && !parse_dig(&out).is_empty())
}

// Return the first nameserver of the primary (unscoped, domain-less) resolver.
pub fn primary_nameserver(resolvers: &[Resolver]) -> Option<&str> {
    resolvers
        .iter()
        .find(|r| !r.scoped && r.domain.is_none() && !r.nameservers.is_empty())
        .map(|r| r.nameservers[0].as_str())
}

// Return the interface a route lookup picks for an address.
fn route_if(ip: &IpAddr) -> Option<String> {
    let text = ip.to_string();
    let mut cmd = vec!["route", "-n", "get"];
    if ip.is_ipv6() {
        cmd.push("-inet6");
    }
    cmd.push(&text);
    parse_route_if(&sh(&cmd).1)
}

// Return seconds since the last wake from sleep, if the Mac has slept.
fn wake_age() -> Option<u64> {
    let woke = parse_waketime(&sh(&["sysctl", "-n", "kern.waketime"]).1)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(woke))
}

// Probe the public internet in layers: captive portal, IP path, DNS and HTTPS.
fn public_probes(f: &Facts) -> Public {
    let v4: IpAddr = PUBLIC_V4.parse().expect("valid literal");
    let v6: IpAddr = PUBLIC_V6.parse().expect("valid literal");
    let captive_url = "http://captive.apple.com/hotspot-detect.html";
    let (rc, body) = sh_opts(&["curl", "-sS", "--max-time", "8", captive_url], 12, false);
    let https = ["curl", "-sS", "-o", "/dev/null", "--max-time", "8", "https://www.apple.com/"];
    let ip6 = f.link.ipv6_global.then(|| tcp_any(&[v6], 443, 4));
    let ip_ok = if f.link.ipv4.is_some() { tcp_any(&[v4], 443, 4) } else { ip6 == Some(true) };
    let dns_ok = resolve("www.apple.com", 6).is_ok();
    let direct_dns_ok = if dns_ok {
        None
    } else {
        primary_nameserver(&f.resolvers).and_then(|ns| direct_lookup(ns, "www.apple.com"))
    };
    Public {
        captive: rc == 0 && !body.contains("Success"),
        ip_ok,
        ip6_ok: if f.link.ipv4.is_some() { ip6 } else { None },
        dns_ok,
        direct_dns_ok,
        https_ok: sh_opts(&https, 12, false).0 == 0,
    }
}

// Probe an internal host: name, the resolver that should answer, route and TCP.
fn probe_internal(t: &Target, resolvers: &[Resolver]) -> Internal {
    let addrs = resolve(&t.host, 6);
    let resolver = match_resolver(resolvers, &t.host).cloned();
    let direct_ok = match (&addrs, &resolver) {
        (Ok(_), _) => None,
        (Err(_), Some(r)) => direct_lookup(&r.nameservers[0], &t.host),
        (Err(_), None) => primary_nameserver(resolvers).and_then(|ns| direct_lookup(ns, &t.host)),
    };
    let first = addrs.as_ref().ok().and_then(|a| a.first().copied());
    let tcp_ok = match (&addrs, t.port) {
        (Ok(a), Some(p)) => Some(tcp_any(a, p, 6)),
        _ => None,
    };
    Internal {
        target: t.clone(),
        route_if: first.as_ref().and_then(route_if),
        addrs,
        tcp_ok,
        resolver,
        direct_ok,
    }
}

// Record the link, gateway and Wi-Fi facts of the chosen physical service.
fn link_facts(f: &mut Facts, all_ifaces: &[String]) {
    let Some(p) = f.phys.clone() else {
        return;
    };
    f.link = parse_ifconfig(&sh(&["ifconfig", &p.device]).1);
    if p.is_wifi() {
        let power = sh(&["networksetup", "-getairportpower", &p.device]).1;
        f.wifi_power = Some(power.trim_end().ends_with("On"));
        f.wifi_missing = !all_ifaces.contains(&p.device);
    }
    f.dhcp = sh(&["networksetup", "-getinfo", &p.name]).1.contains("DHCP Configuration");
    let gw = sh(&["ipconfig", "getoption", &p.device, "router"]).1;
    if gw.parse::<std::net::Ipv4Addr>().is_ok() {
        f.gateway_ok = Some(sh_opts(&["ping", "-c", "2", "-t", "4", &gw], 10, false).0 == 0);
        f.gateway = Some(gw);
    }
}

// Record which enabled proxies and PAC file are unreachable.
fn proxy_facts(f: &mut Facts) {
    f.proxy = parse_proxy(&sh(&["scutil", "--proxy"]).1);
    for (kind, server) in [("HTTP", &f.proxy.http), ("HTTPS", &f.proxy.https)] {
        if let Some((host, port)) = server {
            if !tcp(host, *port, 3) {
                f.proxy_down.push(format!("{kind} proxy {host}:{port}"));
            }
        }
    }
    if let Some(url) = &f.proxy.pac {
        f.pac_ok =
            Some(sh_opts(&["curl", "-sS", "-o", "/dev/null", "--max-time", "5", url], 8, false).0 == 0);
    }
}

// Record scutil-managed VPN services and the tunnel interface of connected ones.
fn vpn_facts(f: &mut Facts) {
    f.vpns = parse_vpn_list(&sh(&["scutil", "--nc", "list"]).1);
    for v in f.vpns.iter_mut().filter(|v| v.state != "Disconnected") {
        let (state, iface) = parse_vpn_status(&sh(&["scutil", "--nc", "status", &v.name]).1);
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
pub fn gather(targets: &[Target]) -> Facts {
    let (rc_order, order) = sh(&["networksetup", "-listnetworkserviceorder"]);
    let (rc_dns, dns) = sh(&["scutil", "--dns"]);
    let all_ifaces = words(&sh(&["ifconfig", "-l"]).1);
    let mut f = Facts {
        macos: sh(&["sw_vers", "-productVersion"]).1,
        over_ssh: std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some(),
        config_hung: rc_order == 124 || rc_dns == 124,
        wake_age: wake_age(),
        up_ifaces: words(&sh(&["ifconfig", "-l", "-u"]).1),
        routes: parse_routes(&sh(&["netstat", "-rn"]).1),
        default_if: parse_route_if(&sh(&["route", "-n", "get", "default"]).1)
            .or_else(|| parse_route_if(&sh(&["route", "-n", "get", "-inet6", "default"]).1)),
        public_if: parse_route_if(&sh(&["route", "-n", "get", PUBLIC_V4]).1),
        resolvers: parse_dns(&dns),
        ..Facts::default()
    };
    let candidates: Vec<(Service, IfStatus)> = parse_services(&order)
        .into_iter()
        .map(|s| {
            let st = parse_ifconfig(&sh(&["ifconfig", &s.device]).1);
            (s, st)
        })
        .collect();
    f.phys = choose_phys(&candidates, f.default_if.as_deref()).map(|i| candidates[i].0.clone());
    link_facts(&mut f, &all_ifaces);
    proxy_facts(&mut f);
    vpn_facts(&mut f);
    f.public = public_probes(&f);
    f.internal = targets.iter().map(|t| probe_internal(t, &f.resolvers)).collect();
    f
}

// Unit tests for snapshot helpers.
#[cfg(test)]
mod tests {
    use super::*;

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

    // Check that only tunnels with real routes count, and scoped defaults do not.
    #[test]
    fn test_tunnels_ignore_system_utuns() {
        let f = Facts {
            routes: vec![
                route("default", "UGcIg", "utun0"),
                route("10/8", "UCS", "utun4"),
                route("default", "UGSc", "en0"),
            ],
            ..Facts::default()
        };
        assert_eq!(f.tunnels(), vec!["utun4"]);
        assert!(f.tunnel_defaults().is_empty());
    }

    // Check that the longest split-DNS domain wins.
    #[test]
    fn test_match_resolver_longest_suffix() {
        let rs = vec![split("corp.example", "utun4"), split("eu.corp.example", "utun4")];
        let hit = match_resolver(&rs, "wiki.eu.corp.example").unwrap();
        assert_eq!(hit.domain.as_deref(), Some("eu.corp.example"));
        assert!(match_resolver(&rs, "example.org").is_none());
        assert!(match_resolver(&rs, "notcorp.example").is_none());
    }

    // Check that resolvers and routes on vanished interfaces are reported stale.
    #[test]
    fn test_stale_state_after_tunnel_gone() {
        let f = Facts {
            up_ifaces: vec!["en0".into(), "lo0".into()],
            routes: vec![route("10/8", "UCS", "utun4"), route("default", "UGSc", "en0")],
            resolvers: vec![split("corp.example", "utun4")],
            ..Facts::default()
        };
        assert_eq!(f.stale_routes().len(), 1);
        assert_eq!(f.stale_resolvers().len(), 1);
    }

    // Check the physical-service choice for Ethernet, Wi-Fi and empty cases.
    #[test]
    fn test_choose_phys() {
        let wifi = Service { name: "Wi-Fi".into(), port: "Wi-Fi".into(), device: "en0".into() };
        let lan = Service { name: "LAN".into(), port: "Ethernet".into(), device: "en7".into() };
        let up = IfStatus { active: true, ipv4: Some("10.0.0.2".into()), ipv6_global: false };
        let down = IfStatus::default();
        let c = vec![(wifi.clone(), down.clone()), (lan.clone(), up.clone())];
        assert_eq!(choose_phys(&c, Some("utun4")), Some(1));
        assert_eq!(choose_phys(&c, Some("en0")), Some(0));
        assert_eq!(choose_phys(&[(lan, down.clone()), (wifi, down)], None), Some(1));
        assert_eq!(choose_phys(&[], None), None);
    }

    // Check that an IP literal resolves without a lookup.
    #[test]
    fn test_resolve_literal() {
        assert_eq!(resolve("10.1.2.3", 1).unwrap(), vec!["10.1.2.3".parse::<IpAddr>().unwrap()]);
    }
}
