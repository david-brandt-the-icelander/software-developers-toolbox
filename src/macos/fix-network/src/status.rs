// Fast read-only health summary for a prompt or script: Wi-Fi, IP, DNS, internet reachability and VPN.
use std::net::IpAddr;

use serde_json::{json, Map, Value};

use crate::facts::{choose_phys, service_links, vpn_services, Facts, PUBLIC_V4, PUBLIC_V6};
use crate::parse::{parse_route_if, parse_routes, IfStatus, Service, VpnService};
use crate::run::c;
use crate::sys::Sys;

// Seconds each command may take.
const CMD_SECS: u64 = 2;
// Seconds the name lookup and each connection may take, so a dead network answers within about two seconds.
const PROBE_SECS: u64 = 1;

// What the status probes saw.
#[derive(Debug, Default)]
pub struct Status {
    pub phys: Option<Service>,
    pub link: IfStatus,
    pub wifi_on: Option<bool>,
    pub dns: bool,
    pub internet: bool,
    pub vpns: Vec<VpnService>,
    pub tunnels: Vec<String>,
}

// One area's verdict: healthy or not, its words on the summary line and its JSON fields.
struct Verdict {
    name: &'static str,
    ok: bool,
    text: String,
    json: Value,
}

// Look a public name up and connect to TCP 443 on a public address, else on the looked-up one; return (dns, internet).
fn reach(sys: &dyn Sys, link: &IfStatus) -> (bool, bool) {
    let looked = sys.resolve("www.apple.com", PROBE_SECS);
    let public = if link.ipv4.is_some() { PUBLIC_V4 } else { PUBLIC_V6 };
    let public: IpAddr = public.parse().expect("valid literal");
    let internet =
        sys.tcp(&[public], 443, PROBE_SECS) || looked.as_ref().is_ok_and(|a| sys.tcp(a, 443, PROBE_SECS));
    (looked.is_ok(), internet)
}

// Take the quick snapshot: the service in use, its address, the Wi-Fi radio, DNS, internet, VPNs and tunnels.
pub fn probe(sys: &dyn Sys) -> Status {
    let out = |cmd: &[&str]| sys.run(cmd, CMD_SECS, false).1;
    let default_if = parse_route_if(&out(&["route", "-n", "get", "default"]));
    let links = service_links(sys, &out(&["networksetup", "-listnetworkserviceorder"]), CMD_SECS);
    let chosen = choose_phys(&links, default_if.as_deref()).map(|i| links[i].clone());
    let (phys, link) = chosen.map_or((None, IfStatus::default()), |(s, l)| (Some(s), l));
    let wifi_on = phys
        .as_ref()
        .filter(|p| p.is_wifi())
        .map(|p| out(&["networksetup", "-getairportpower", &p.device]).trim_end().ends_with("On"));
    let has_addr = link.ipv4.is_some() || link.ipv6_global;
    let (dns, internet) = if has_addr { reach(sys, &link) } else { (false, false) };
    let routes = Facts { routes: parse_routes(&out(&["netstat", "-rn"])), ..Facts::default() };
    Status {
        phys,
        link,
        wifi_on,
        dns,
        internet,
        vpns: vpn_services(sys, CMD_SECS),
        tunnels: routes.tunnels(),
    }
}

// Judge the Wi-Fi radio of the service in use; a wired service leaves Wi-Fi unused.
fn wifi(s: &Status) -> Verdict {
    let device = s.phys.as_ref().map(|p| p.device.clone());
    let (ok, state) = match s.wifi_on {
        None => (true, "not used"),
        Some(true) => (true, "on"),
        Some(false) => (false, "off"),
    };
    Verdict {
        name: "wifi",
        ok,
        text: format!("Wi-Fi {state}"),
        json: json!({"ok": ok, "state": state, "device": device}),
    }
}

// Judge the address of the service in use.
fn ip(s: &Status) -> Verdict {
    let l = &s.link;
    let ok = l.ipv4.is_some() || l.ipv6_global;
    let dev = s.phys.as_ref().map_or("-", |p| p.device.as_str());
    let text = match (&l.ipv4, ok) {
        (Some(v4), _) => format!("IP {v4} ({dev})"),
        (None, true) => format!("IP v6 only ({dev})"),
        (None, false) => "IP none".to_string(),
    };
    let service = s.phys.as_ref().map(|p| p.name.clone());
    let json = json!({"ok": ok, "interface": s.phys.as_ref().map(|p| p.device.clone()), "service": service,
                      "ipv4": l.ipv4, "ipv6_global": l.ipv6_global});
    Verdict { name: "ip", ok, text, json }
}

// Judge a probe that either worked or failed.
fn probed(name: &'static str, label: &str, ok: bool) -> Verdict {
    let word = if ok { "ok" } else { "FAILED" };
    Verdict { name, ok, text: format!("{label} {word}"), json: json!({"ok": ok}) }
}

// Judge VPNs: one stuck between states is degraded; a connected one, a third-party tunnel or none is fine.
fn vpn(s: &Status) -> Verdict {
    let verdict = |ok: bool, text: String, state: &str, name: Option<&str>, iface: Option<&str>| Verdict {
        name: "vpn",
        ok,
        text,
        json: json!({"ok": ok, "state": state, "name": name, "interface": iface}),
    };
    if let Some(v) = s.vpns.iter().find(|v| v.state != "Connected" && v.state != "Disconnected") {
        let state = v.state.to_lowercase();
        return verdict(false, format!("VPN {} {state}", v.name), &state, Some(&v.name), v.iface.as_deref());
    }
    if let Some(v) = s.vpns.iter().find(|v| v.state == "Connected") {
        let iface = v.iface.as_deref();
        let text = format!("VPN {} ({})", v.name, iface.unwrap_or("?"));
        return verdict(true, text, "connected", Some(&v.name), iface);
    }
    if let Some(t) = s.tunnels.first() {
        return verdict(true, format!("VPN tunnel {t}"), "tunnel", None, Some(t));
    }
    verdict(true, "VPN none".to_string(), "none", None, None)
}

// Judge every area in summary-line order.
fn verdicts(s: &Status) -> Vec<Verdict> {
    vec![wifi(s), ip(s), probed("dns", "DNS", s.dns), probed("internet", "internet", s.internet), vpn(s)]
}

// Render the one-line summary.
pub fn line(s: &Status) -> String {
    let all = verdicts(s);
    let head = if all.iter().all(|v| v.ok) { c("32", "network ok") } else { c("31", "network DEGRADED") };
    let parts: Vec<String> = all.into_iter().map(|v| v.text).collect();
    format!("{head} | {}", parts.join(" | "))
}

// Render the summary as a JSON object with one field per area.
pub fn to_json(s: &Status) -> Value {
    let all = verdicts(s);
    let mut out = Map::new();
    out.insert("tool".into(), json!("fix-network"));
    out.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    out.insert("ok".into(), json!(all.iter().all(|v| v.ok)));
    for v in all {
        out.insert(v.name.into(), v.json);
    }
    Value::Object(out)
}

// Probe, print the line or JSON, and tell whether every area is healthy.
pub fn cmd_status(sys: &dyn Sys, as_json: bool) -> bool {
    let s = probe(sys);
    if as_json {
        println!("{}", to_json(&s));
    } else {
        println!("{}", line(&s));
    }
    verdicts(&s).iter().all(|v| v.ok)
}

// Tests for the quick status against a scripted Mac.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::facts::tests::{mac, with_vpn};

    // Check a healthy Wi-Fi Mac: every area ok, no slow or privileged command run.
    #[test]
    fn test_status_healthy() {
        let sys = mac();
        let s = probe(&sys);
        assert_eq!(
            line(&s),
            "network ok | Wi-Fi on | IP 192.168.1.5 (en0) | DNS ok | internet ok | VPN none"
        );
        let j = to_json(&s);
        assert_eq!((j["tool"].as_str(), j["ok"].as_bool()), (Some("fix-network"), Some(true)));
        assert_eq!(j["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(j["wifi"]["state"], "on");
        assert_eq!(j["ip"]["ipv4"], "192.168.1.5");
        assert_eq!(j["vpn"]["state"], "none");
        assert!(cmd_status(&sys, true) && cmd_status(&sys, false));
        let calls = sys.calls.borrow();
        assert!(calls
            .iter()
            .all(|c| !c.starts_with("sudo") && !c.starts_with("ping") && !c.starts_with("curl")));
    }

    // Check a Wi-Fi radio that is off and has no address: degraded, and no lookup is tried.
    #[test]
    fn test_status_wifi_off_no_address() {
        let sys = mac();
        sys.on("networksetup -getairportpower en0", &[(0, "Wi-Fi Power (en0): Off")])
            .on("ifconfig en0", &[(0, "en0: flags=8863<UP>\n\tstatus: inactive\n")]);
        let s = probe(&sys);
        assert!(!s.dns && !s.internet);
        assert_eq!(
            line(&s),
            "network DEGRADED | Wi-Fi off | IP none | DNS FAILED | internet FAILED | VPN none"
        );
        assert!(!cmd_status(&sys, false));
        assert_eq!(to_json(&s)["wifi"]["ok"], false);
    }

    // Check a wired, IPv6-only Mac whose raw-IP probe is blocked but HTTPS to the looked-up name connects.
    #[test]
    fn test_status_wired_ipv6_only() {
        let sys = mac();
        let order = "(1) Ethernet\n(Hardware Port: Ethernet, Device: en7)\n";
        sys.on("networksetup -listnetworkserviceorder", &[(0, order)])
            .on("route -n get default", &[(0, "  interface: en7")])
            .on(
                "ifconfig en7",
                &[(0, "en7: flags=8863<UP>\n\tinet6 2001:db8::5 prefixlen 64\n\tstatus: active\n")],
            )
            .listen("17.253.144.10", 443);
        let s = probe(&sys);
        assert_eq!(s.wifi_on, None);
        assert_eq!(
            line(&s),
            "network ok | Wi-Fi not used | IP v6 only (en7) | DNS ok | internet ok | VPN none"
        );
        assert_eq!(sys.count("networksetup -getairportpower en7"), 0);
    }

    // Check DNS failing while the raw-IP path works, and the internet failing too.
    #[test]
    fn test_status_dns_and_internet_failures() {
        let sys = mac();
        sys.names.borrow_mut().clear();
        let s = probe(&sys);
        assert!(!s.dns && s.internet);
        assert!(line(&s).contains("DNS FAILED | internet ok"));
        sys.open.borrow_mut().clear();
        assert!(!probe(&sys).internet);
        sys.on("networksetup -listnetworkserviceorder", &[(0, "")]);
        let s = probe(&sys);
        assert!(s.phys.is_none() && line(&s).contains("Wi-Fi not used | IP none"));
        assert_eq!(to_json(&s)["ip"]["interface"], Value::Null);
    }

    // Check VPN verdicts: connected, connected without an interface, stuck, and a third-party tunnel.
    #[test]
    fn test_status_vpn_states() {
        let sys = with_vpn(mac());
        let s = probe(&sys);
        assert!(line(&s).ends_with("VPN Work (utun4)"));
        assert_eq!(to_json(&s)["vpn"]["interface"], "utun4");
        sys.on("scutil --nc status Work", &[(0, "Connected\n")]);
        assert!(line(&probe(&sys)).ends_with("VPN Work (?)"));
        sys.on("scutil --nc status Work", &[(0, "Connecting\n")]);
        let s = probe(&sys);
        assert!(line(&s).starts_with("network DEGRADED") && line(&s).ends_with("VPN Work connecting"));
        assert_eq!(to_json(&s)["vpn"]["state"], "connecting");
        sys.on("scutil --nc list", &[(0, "Available network connection services:\n")]);
        let s = probe(&sys);
        assert!(line(&s).ends_with("VPN tunnel utun4"));
        assert_eq!(to_json(&s)["vpn"]["ok"], true);
    }
}
