// Corporate DNS, corporate service and proxy checks.
use std::net::IpAddr;

use crate::assess::{check, wrong_route, Ctx};
use crate::facts::Internal;
use crate::model::{Check, Remedy, State, Step};
use crate::parse::{is_private, is_tunnel};

// Explain why an internal name does not resolve.
fn corp_dns_failure(cx: &mut Ctx, i: &Internal) {
    let f = cx.f;
    let host = &i.target.host;
    let reboot = Remedy::Reboot(format!("The system resolver still fails for {host} after a flush: reboot."));
    match (&i.resolver, i.direct_ok) {
        (Some(r), Some(true)) => {
            let ns = &r.nameservers[0];
            let remedy = cx.steps(vec![Step::FlushDns], reboot);
            cx.add(
                "corp-dns-resolver",
                format!("{host}: {ns} answers directly but the system resolver fails"),
                remedy,
            );
        }
        (Some(r), _) => {
            let via = r.iface.as_deref().unwrap_or("?");
            let remedy = cx.vpn_fix("The VPN's DNS server is unreachable: reconnect the VPN client; if it persists, ask the VPN admin.");
            cx.add(
                "corp-dns-server",
                format!("{host}: corporate DNS server {} (via {via}) does not answer", r.nameservers[0]),
                remedy,
            );
        }
        (None, Some(true)) => {
            let remedy = cx.steps(vec![Step::FlushDns], reboot);
            cx.add(
                "corp-dns-resolver",
                format!("{host}: the DNS server answers directly but the system resolver fails"),
                remedy,
            );
        }
        (None, _) if f.vpn_present() => {
            let remedy = cx.vpn_fix("Reconnect the VPN client so it installs its DNS settings; if it persists, ask the VPN admin.");
            cx.add(
                "corp-dns-missing",
                format!("{host}: no DNS resolver for its domain (the VPN set up no split DNS for it)"),
                remedy,
            );
        }
        (None, _) => {
            let remedy = cx.vpn_connect();
            cx.add("corp-no-vpn", format!("{host} does not resolve and no VPN is connected"), remedy);
        }
    }
}

// Check corporate DNS: stale split resolvers and each internal name.
pub fn corp_dns(cx: &mut Ctx) -> Check {
    let name = "Corporate DNS";
    let f = cx.f;
    let mut failed = Vec::new();
    let stale: Vec<String> = f
        .stale_resolvers()
        .iter()
        .map(|r| {
            format!(
                "{} ({} via {})",
                r.domain.as_deref().unwrap_or("?"),
                r.nameservers[0],
                r.iface.as_deref().unwrap_or("?")
            )
        })
        .collect();
    if !stale.is_empty() {
        let text = "Split-DNS resolvers from an old VPN session remain: quit the VPN app; if they persist, reboot clears them.";
        let remedy = match f.connected_vpn() {
            Some(v) => cx.steps(vec![Step::ReconnectVpn(v.name.clone())], Remedy::Reboot(text.into())),
            None => Remedy::Reboot(text.into()),
        };
        cx.add(
            "stale-dns",
            format!("Split-DNS resolvers point at a dead tunnel or unreachable server: {}", stale.join(", ")),
            remedy,
        );
        failed.push("stale split-DNS resolvers".to_string());
    }
    let names: Vec<&Internal> =
        f.internal.iter().filter(|i| i.target.host.parse::<IpAddr>().is_err()).collect();
    let mut ok = Vec::new();
    for i in names.iter().copied() {
        let host = &i.target.host;
        match &i.addrs {
            Err(e) => {
                failed.push(format!("{host}: {e}"));
                if f.public.dns_ok {
                    corp_dns_failure(cx, i);
                }
            }
            Ok(a) if a.iter().any(|ip| ip.is_unspecified() || ip.is_loopback()) => {
                let text = format!("A DNS filter or /etc/hosts entry answers for {host}: check them.");
                cx.add(
                    "corp-dns-blocked",
                    format!("{host} resolves to {} (blocked or sinkholed)", a[0]),
                    Remedy::Manual(text),
                );
                failed.push(format!("{host} -> {}", a[0]));
            }
            Ok(a) => {
                let public = f.vpn_present() && a.iter().all(|ip| !is_private(ip));
                ok.push(format!("{host} -> {}{}", a[0], if public { " (public address)" } else { "" }));
            }
        }
    }
    if !failed.is_empty() {
        return check(name, State::Failed, failed.join("; "));
    }
    if names.is_empty() {
        let domains: Vec<&str> =
            f.resolvers.iter().filter(|r| r.supplemental()).filter_map(|r| r.domain.as_deref()).collect();
        let hint = if domains.is_empty() {
            String::new()
        } else {
            format!(" (split domains: {})", domains.join(", "))
        };
        let why =
            if f.internal.is_empty() { "pass --internal-host HOST[:PORT]" } else { "only IP targets given" };
        return check(name, State::NotTested, format!("{why}{hint}"));
    }
    check(name, State::Healthy, ok.join("; "))
}

// Explain why an internal service refuses or drops connections.
fn corp_service_failure(cx: &mut Ctx, i: &Internal) {
    let f = cx.f;
    let target = format!("{}:{}", i.target.host, i.target.port.unwrap_or(0));
    let via = i.route_if.clone().unwrap_or("?".into());
    let private = i.addrs.as_ref().is_ok_and(|a| a.iter().any(is_private));
    if is_tunnel(&via) {
        let text = format!(
            "The tunnel passes no traffic to {target}: reconnect the VPN client; if it persists, the service \
             or a firewall is down (not fixable here)."
        );
        let remedy = cx.vpn_fix(&text);
        cx.add("corp-service-vpn", format!("{target} unreachable through tunnel {via}"), remedy);
    } else if private && !f.vpn_present() {
        let remedy = cx.vpn_connect();
        cx.add(
            "corp-no-vpn",
            format!("{target} (private address) unreachable and no VPN is connected"),
            remedy,
        );
    } else {
        let text = "The service, its firewall or the path to it is down; nothing on this Mac to fix.";
        cx.add(
            "corp-service",
            format!("{target} refuses or drops connections via {via}"),
            Remedy::Manual(text.into()),
        );
    }
}

// Check TCP reachability of internal services given with a port.
pub fn corp_service(cx: &mut Ctx) -> Check {
    let name = "Corporate service";
    let f = cx.f;
    let with_port: Vec<&Internal> = f.internal.iter().filter(|i| i.target.port.is_some()).collect();
    if with_port.is_empty() {
        let why = if f.internal.is_empty() { "pass --internal-host HOST:PORT" } else { "no port given" };
        return check(name, State::NotTested, why);
    }
    let (mut ok, mut failed, mut untested) = (Vec::new(), Vec::new(), Vec::new());
    for i in with_port {
        let target = format!("{}:{}", i.target.host, i.target.port.unwrap_or(0));
        match i.tcp_ok {
            None => untested.push(format!("{target} not tested (no address)")),
            Some(true) => ok.push(format!("{target} open")),
            Some(false) => {
                failed.push(format!("{target} unreachable"));
                if f.public.ip_ok && wrong_route(f, i).is_none() {
                    corp_service_failure(cx, i);
                }
            }
        }
    }
    let detail = [failed.clone(), untested.clone(), ok.clone()].concat().join("; ");
    let state = match (failed.is_empty(), ok.is_empty()) {
        (false, _) => State::Failed,
        (true, false) => State::Healthy,
        (true, true) => State::NotTested,
    };
    check(name, state, detail)
}

// Check that enabled system proxies and the PAC file are reachable.
pub fn proxy(cx: &mut Ctx) -> Check {
    let name = "Proxy";
    let f = cx.f;
    let p = &f.proxy;
    if p.http.is_none() && p.https.is_none() && p.pac.is_none() {
        return check(name, State::Absent, "no system proxy");
    }
    let mut failed = Vec::new();
    if !f.proxy_down.is_empty() {
        let text = "Fix or disable the proxy in System Settings > Network > Details > Proxies \
                    (it may only be reachable on the VPN, or be managed by a profile).";
        let remedy = cx.steps(vec![Step::FixProxy], Remedy::Manual(text.into()));
        cx.add(
            "proxy-down",
            format!("{} unreachable: apps that use the system proxy fail", f.proxy_down.join(", ")),
            remedy,
        );
        failed.extend(f.proxy_down.clone());
    }
    if f.pac_ok == Some(false) {
        let url = p.pac.clone().unwrap_or_default();
        let text =
            "The PAC file may only be reachable on the VPN; fix or remove it in System Settings > Network > \
                    Details > Proxies.";
        cx.add(
            "pac-down",
            format!("Proxy auto-config file {url} is unreachable"),
            Remedy::Manual(text.into()),
        );
        failed.push(format!("PAC {url} unreachable"));
    }
    if !failed.is_empty() {
        return check(name, State::Failed, failed.join("; "));
    }
    let mut on: Vec<String> = [("HTTP", &p.http), ("HTTPS", &p.https)]
        .iter()
        .filter_map(|(k, s)| s.as_ref().map(|(h, port)| format!("{k} {h}:{port}")))
        .collect();
    on.extend(p.pac.iter().map(|u| format!("PAC {u}")));
    check(name, State::Healthy, format!("{} reachable", on.join(", ")))
}
