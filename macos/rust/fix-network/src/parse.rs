// Pure parsers for macOS network command output and CLI targets.
use std::net::IpAddr;

use regex::Regex;

use crate::run::capture;

// One network service from networksetup -listnetworkserviceorder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Service {
    pub name: String,
    pub port: String,
    pub device: String,
}

impl Service {
    // Tell whether the service is the Wi-Fi hardware port.
    pub fn is_wifi(&self) -> bool {
        self.port == "Wi-Fi" || self.port == "AirPort"
    }
}

// Address facts for one interface from ifconfig.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IfStatus {
    pub active: bool,
    pub ipv4: Option<String>,
    pub ipv6_global: bool,
}

// One routing-table entry from netstat -rn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Route {
    pub v6: bool,
    pub dest: String,
    pub gateway: String,
    pub flags: String,
    pub netif: String,
}

impl Route {
    // Tell whether the route is bound to one interface rather than system-wide.
    pub fn scoped(&self) -> bool {
        self.flags.contains('I')
    }

    // Tell whether the route only covers link-local, broadcast or multicast space.
    pub fn link_local(&self) -> bool {
        let d = self.dest.to_ascii_lowercase();
        if self.v6 {
            return d.starts_with("fe80") || d.starts_with("ff");
        }
        d.starts_with("224") || d.starts_with("169.254") || d.starts_with("255.255.255.255")
    }
}

// One resolver block from scutil --dns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolver {
    pub scoped: bool,
    pub domain: Option<String>,
    pub search: Vec<String>,
    pub nameservers: Vec<String>,
    pub iface: Option<String>,
    pub reachable: Option<bool>,
}

impl Resolver {
    // Tell whether this is a split-DNS resolver for a corporate-style domain.
    pub fn supplemental(&self) -> bool {
        let Some(d) = self.domain.as_deref() else {
            return false;
        };
        !self.scoped && !self.nameservers.is_empty() && !d.ends_with("arpa") && d != "local"
    }
}

// Enabled system proxies from scutil --proxy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Proxy {
    pub http: Option<(String, u16)>,
    pub https: Option<(String, u16)>,
    pub pac: Option<String>,
}

// One VPN service from scutil --nc list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VpnService {
    pub name: String,
    pub state: String,
    pub iface: Option<String>,
}

// An internal host and optional port given on the command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub host: String,
    pub port: Option<u16>,
}

// Tell whether an interface name belongs to a tunnel.
pub fn is_tunnel(name: &str) -> bool {
    ["utun", "ipsec", "ppp", "tun", "tap", "wg"].iter().any(|p| name.starts_with(p))
}

// Tell whether an address is private, carrier-grade NAT or unique-local.
pub fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

// Parse HOST, HOST:PORT, [V6]:PORT or a bare IPv6 literal.
pub fn parse_target(arg: &str) -> Result<Target, String> {
    let port = |p: &str| p.parse::<u16>().map_err(|_| format!("bad port in {arg}"));
    if let Some(rest) = arg.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or(format!("missing ] in {arg}"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(port(p)?),
            None if tail.is_empty() => None,
            None => return Err(format!("bad target {arg}")),
        };
        return Ok(Target { host: host.to_string(), port });
    }
    if arg.matches(':').count() == 1 {
        let (host, p) = arg.split_once(':').expect("one colon");
        return Ok(Target { host: host.to_string(), port: Some(port(p)?) });
    }
    if arg.is_empty() {
        return Err("empty host".to_string());
    }
    Ok(Target { host: arg.to_string(), port: None })
}

// Parse networksetup -listnetworkserviceorder into enabled services with a device.
pub fn parse_services(text: &str) -> Vec<Service> {
    let re =
        Regex::new(r"\((\d+|\*)\)\s+(.+)\n\(Hardware Port: ([^,]+), Device: ([^)]*)\)").expect("valid regex");
    re.captures_iter(text)
        .filter(|m| &m[1] != "*" && !m[4].trim().is_empty())
        .map(|m| Service {
            name: m[2].trim().to_string(),
            port: m[3].trim().to_string(),
            device: m[4].trim().to_string(),
        })
        .collect()
}

// Parse ifconfig output for one interface.
pub fn parse_ifconfig(text: &str) -> IfStatus {
    let mut st = IfStatus::default();
    for line in text.lines().map(str::trim) {
        let mut words = line.split_whitespace();
        match (words.next(), words.next()) {
            (Some("status:"), Some(s)) => st.active = s == "active",
            (Some("inet"), Some(a)) if st.ipv4.is_none() && !a.starts_with("169.254.") => {
                st.ipv4 = Some(a.to_string())
            }
            (Some("inet6"), Some(a)) => {
                let a = a.to_ascii_lowercase();
                let local = ["fe80", "fd", "fc", "::1"].iter().any(|p| a.starts_with(p));
                st.ipv6_global |= !local;
            }
            _ => {}
        }
    }
    st
}

// Parse netstat -rn into IPv4 and IPv6 routes.
pub fn parse_routes(text: &str) -> Vec<Route> {
    let mut out = Vec::new();
    let (mut v6, mut netif_col) = (false, None);
    for line in text.lines().map(str::trim) {
        if line.starts_with("Internet6") || line.starts_with("Internet") {
            v6 = line.starts_with("Internet6");
            netif_col = None;
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.first() == Some(&"Destination") {
            netif_col = cols.iter().position(|c| *c == "Netif");
            continue;
        }
        let Some(i) = netif_col.filter(|i| cols.len() > *i && *i >= 3) else {
            continue;
        };
        out.push(Route {
            v6,
            dest: cols[0].to_string(),
            gateway: cols[1].to_string(),
            flags: cols[2].to_string(),
            netif: cols[i].to_string(),
        });
    }
    out
}

// Return the interface named by route -n get output.
pub fn parse_route_if(text: &str) -> Option<String> {
    capture(r"interface:\s*(\S+)", text)
}

// Parse scutil --dns into resolvers, marking the scoped-query section.
pub fn parse_dns(text: &str) -> Vec<Resolver> {
    let mut out: Vec<Resolver> = Vec::new();
    let mut scoped = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("DNS configuration") {
            scoped = line.contains("scoped");
            continue;
        }
        if line.starts_with("resolver #") {
            out.push(Resolver { scoped, ..Resolver::default() });
            continue;
        }
        let (Some(r), Some((key, value))) = (out.last_mut(), line.split_once(':')) else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "domain" => r.domain = Some(value.trim_end_matches('.').to_string()),
            "if_index" => r.iface = capture(r"\((\S+)\)", value),
            "reach" => r.reachable = Some(!value.contains("Not Reachable")),
            k if k.starts_with("nameserver") => r.nameservers.push(value.to_string()),
            k if k.starts_with("search domain") => r.search.push(value.to_string()),
            _ => {}
        }
    }
    out
}

// Parse scutil --proxy into the enabled proxies.
pub fn parse_proxy(text: &str) -> Proxy {
    let get = |key: &str| capture(&format!(r"(?m)^\s*{key}\s*:\s*(\S+)"), text);
    let on = |key: &str| get(key).as_deref() == Some("1");
    let server = |kind: &str, default: u16| {
        let host = get(&format!("{kind}Proxy"))?;
        let port = get(&format!("{kind}Port")).and_then(|p| p.parse().ok()).unwrap_or(default);
        on(&format!("{kind}Enable")).then_some((host, port))
    };
    Proxy {
        http: server("HTTP", 80),
        https: server("HTTPS", 443),
        pac: get("ProxyAutoConfigURLString").filter(|_| on("ProxyAutoConfigEnable")),
    }
}

// Parse scutil --nc list into VPN services and their states.
pub fn parse_vpn_list(text: &str) -> Vec<VpnService> {
    let re = Regex::new(r#"\((\w+)\)\s+\S+.*?"([^"]+)""#).expect("valid regex");
    re.captures_iter(text)
        .map(|m| VpnService { name: m[2].to_string(), state: m[1].to_string(), iface: None })
        .collect()
}

// Return the state line and tunnel interface from scutil --nc status.
pub fn parse_vpn_status(text: &str) -> (String, Option<String>) {
    let state = text.lines().next().unwrap_or_default().trim().to_string();
    (state, capture(r"InterfaceName\s*:\s*(\S+)", text))
}

// Return the wake time in Unix seconds from sysctl kern.waketime.
pub fn parse_waketime(text: &str) -> Option<u64> {
    capture(r"sec = (\d+)", text).and_then(|s| s.parse().ok()).filter(|s| *s > 0)
}

// Return the addresses printed by dig +short.
pub fn parse_dig(text: &str) -> Vec<IpAddr> {
    text.lines().filter_map(|l| l.trim().parse().ok()).collect()
}

// Fixture-based parser tests; fixtures follow documented macOS output formats.
#[cfg(test)]
mod tests {
    use super::*;

    const NETSTAT: &str = "Routing tables\n\nInternet:\n\
Destination        Gateway            Flags               Netif Expire\n\
default            192.168.1.1        UGScg                 en0       \n\
default            link#22            UCSIg               utun4       \n\
10/8               link#22            UCS                 utun4       \n\
127                127.0.0.1          UCS                   lo0       \n\
192.168.1          link#6             UCS                   en0      !\n\
224.0.0/4          link#6             UmCS                  en0      !\n\n\
Internet6:\n\
Destination                             Gateway                                 Flags               Netif Expire\n\
default                                 fe80::%utun0                            UGcIg               utun0       \n\
default                                 fe80::1%en0                             UGcg                  en0       \n\
fe80::%utun0/64                         fe80::abcd%utun0                        UcI                 utun0       \n";

    // Check netstat parsing across both families and flag helpers.
    #[test]
    fn test_parse_routes_families() {
        let r = parse_routes(NETSTAT);
        assert_eq!(r.len(), 9);
        assert_eq!(r[1].netif, "utun4");
        assert!(r[1].scoped());
        assert!(!r[0].scoped());
        assert!(r[5].link_local());
        assert!(r[6].v6 && r[6].scoped());
        assert!(r[8].link_local());
    }

    // Check that older netstat output with Refs and Use columns still finds Netif.
    #[test]
    fn test_parse_routes_refs_columns() {
        let text =
            "Internet:\nDestination Gateway Flags Refs Use Netif Expire\ndefault 10.0.0.1 UGSc 12 0 en1\n";
        assert_eq!(parse_routes(text)[0].netif, "en1");
    }

    // Check scutil --dns parsing of supplemental and scoped resolvers.
    #[test]
    fn test_parse_dns_supplemental() {
        let text = "DNS configuration\n\nresolver #1\n  search domain[0] : home\n  nameserver[0] : 192.168.1.1\n  \
                    if_index : 6 (en0)\n  reach    : 0x00020002 (Reachable,Directly Reachable Address)\n\n\
                    resolver #2\n  domain   : corp.example.com\n  nameserver[0] : 10.0.0.53\n  if_index : 22 (utun4)\n  \
                    reach    : 0x00000000 (Not Reachable)\n\nresolver #3\n  domain   : local\n  options  : mdns\n\n\
                    DNS configuration (for scoped queries)\n\nresolver #1\n  nameserver[0] : 192.168.1.1\n  if_index : 6 (en0)\n";
        let r = parse_dns(text);
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].nameservers, vec!["192.168.1.1"]);
        assert!(r[1].supplemental());
        assert_eq!(r[1].iface.as_deref(), Some("utun4"));
        assert_eq!(r[1].reachable, Some(false));
        assert!(!r[2].supplemental());
        assert!(r[3].scoped);
    }

    // Check that IPv6 nameservers keep their colons.
    #[test]
    fn test_parse_dns_ipv6_nameserver() {
        let r = parse_dns("resolver #1\n  nameserver[0] : fe80::1%en0\n");
        assert_eq!(r[0].nameservers, vec!["fe80::1%en0"]);
    }

    // Check scutil --proxy parsing of enabled and disabled proxies.
    #[test]
    fn test_parse_proxy() {
        let text = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 8080\n  HTTPProxy : proxy.corp\n  HTTPSEnable : 0\n  \
                    HTTPSProxy : proxy.corp\n  ProxyAutoConfigEnable : 1\n  ProxyAutoConfigURLString : http://wpad/wpad.dat\n}";
        let p = parse_proxy(text);
        assert_eq!(p.http, Some(("proxy.corp".to_string(), 8080)));
        assert_eq!(p.https, None);
        assert_eq!(p.pac.as_deref(), Some("http://wpad/wpad.dat"));
        assert_eq!(parse_proxy("<dictionary> {\n}"), Proxy::default());
    }

    // Check scutil --nc list and status parsing.
    #[test]
    fn test_parse_vpn() {
        let text = "Available network connection services in the current set (*=enabled):\n\
* (Connected)      0A1B-22 VPN (com.wireguard.macos) \"Work WG\"   [VPN/com.wireguard.macos]\n\
* (Disconnected)   0C3D-44 IPSec              \"Office\"     [IPSec]\n";
        let v = parse_vpn_list(text);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].name.as_str(), v[0].state.as_str()), ("Work WG", "Connected"));
        let status = "Connected\nExtended Status <dictionary> {\n  IPv4 : <dictionary> {\n    InterfaceName : utun5\n  }\n}";
        assert_eq!(parse_vpn_status(status), ("Connected".to_string(), Some("utun5".to_string())));
    }

    // Check service-order parsing skips disabled and device-less services.
    #[test]
    fn test_parse_services() {
        let text = "(1) Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n\n(2) USB LAN\n(Hardware Port: USB 10/100/1000 LAN, Device: en7)\n\n\
                    (*) Bridge\n(Hardware Port: Thunderbolt Bridge, Device: bridge0)\n\n(3) Corp VPN\n(Hardware Port: VPN, Device: )\n";
        let s = parse_services(text);
        assert_eq!(s.len(), 2);
        assert!(s[0].is_wifi());
        assert_eq!(s[1].device, "en7");
    }

    // Check ifconfig parsing for dual-stack, IPv6-only and self-assigned addresses.
    #[test]
    fn test_parse_ifconfig() {
        let dual = "en0: flags=8863<UP>\n\tinet6 fe80::1%en0 prefixlen 64\n\tinet 192.168.1.5 netmask 0xffffff00\n\t\
                    inet6 2001:db8::5 prefixlen 64 autoconf\n\tstatus: active\n";
        assert_eq!(
            parse_ifconfig(dual),
            IfStatus { active: true, ipv4: Some("192.168.1.5".into()), ipv6_global: true }
        );
        let v6only = "en0:\n\tinet6 2001:db8::9 prefixlen 64\n\tstatus: active\n";
        assert_eq!(parse_ifconfig(v6only).ipv4, None);
        assert!(parse_ifconfig(v6only).ipv6_global);
        let apipa = "en0:\n\tinet 169.254.3.4 netmask 0xffff0000\n\tstatus: inactive\n";
        assert_eq!(parse_ifconfig(apipa), IfStatus::default());
    }

    // Check target parsing for names, ports and IPv6 literals.
    #[test]
    fn test_parse_target() {
        assert_eq!(
            parse_target("wiki.corp:443").unwrap(),
            Target { host: "wiki.corp".into(), port: Some(443) }
        );
        assert_eq!(parse_target("wiki.corp").unwrap().port, None);
        assert_eq!(parse_target("[fd00::1]:22").unwrap(), Target { host: "fd00::1".into(), port: Some(22) });
        assert_eq!(parse_target("fd00::1").unwrap().host, "fd00::1");
        assert!(parse_target("host:http").is_err());
    }

    // Check wake time, dig and address classification helpers.
    #[test]
    fn test_small_parsers() {
        assert_eq!(parse_waketime("{ sec = 1731400000, usec = 5 } Tue Nov 12"), Some(1731400000));
        assert_eq!(parse_waketime("{ sec = 0, usec = 0 }"), None);
        assert_eq!(parse_dig(";; timed out\n10.1.2.3\nalias.corp.\n").len(), 1);
        assert!(is_private(&"100.64.1.1".parse().unwrap()));
        assert!(is_private(&"fd12::1".parse().unwrap()));
        assert!(!is_private(&"8.8.8.8".parse().unwrap()));
        assert!(is_tunnel("utun4") && !is_tunnel("en0"));
    }
}
