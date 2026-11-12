// Repair planning, the adaptive fix loop and the repair steps themselves.
use std::path::Path;

use crate::assess::{assess, Remedy, Report, Step};
use crate::facts::{tcp_host, Facts};
use crate::parse::parse_vpn_status;
use crate::run::capture;
use crate::sys::Sys;

// Executes one repair step against a snapshot.
pub type StepFn<'a> = dyn FnMut(&Step, &Facts) -> Result<String, String> + 'a;

// Upper bound on repair rounds so a flapping network cannot loop forever.
const MAX_ROUNDS: usize = 12;

// Flags that permit gated or disruptive steps.
#[derive(Clone, Debug, Default)]
pub struct Gates {
    pub vpn: bool,
    pub fix_proxy: bool,
    pub allow_route_flush: bool,
    pub allow_remote: bool,
    pub over_ssh: bool,
}

impl Gates {
    // Return the missing flag that blocks a step, if any.
    pub fn blocked(&self, s: &Step) -> Option<&'static str> {
        let given = match s.gate() {
            Some("--vpn") => self.vpn,
            Some("--fix-proxy") => self.fix_proxy,
            Some("--allow-route-flush") => self.allow_route_flush,
            _ => true,
        };
        if !given {
            return s.gate();
        }
        (self.over_ssh && s.disruptive() && !self.allow_remote).then_some("--allow-remote")
    }
}

// One executed repair step and its outcome.
#[derive(Clone, Debug)]
pub struct Action {
    pub step: Step,
    pub result: Result<String, String>,
}

// Collect the steps a report calls for, deduplicated and least disruptive first, with blocking flags.
pub fn plan(report: &Report, gates: &Gates) -> Vec<(Step, Option<&'static str>)> {
    let mut steps: Vec<Step> = Vec::new();
    for f in &report.findings {
        let Remedy::Steps(list) = &f.remedy else {
            continue;
        };
        for s in list {
            if !steps.contains(s) {
                steps.push(s.clone());
            }
        }
    }
    steps.sort_by_key(Step::rank);
    steps
        .into_iter()
        .map(|s| {
            let blocked = gates.blocked(&s);
            (s, blocked)
        })
        .collect()
}

// Return the standard ladder that --force runs regardless of health.
pub fn force_steps(f: &Facts) -> Vec<Step> {
    let mut out = vec![Step::FlushDns];
    if f.gateway.is_some() {
        out.push(Step::ClearArp);
    }
    if f.dhcp && f.phys.is_some() {
        out.push(Step::RenewDhcp);
    }
    if f.phys.as_ref().is_some_and(|p| p.is_wifi()) {
        out.push(Step::CycleWifi);
    }
    out
}

// Run one step, log its outcome and record it.
fn act(step: Step, facts: &Facts, exec: &mut StepFn, log: &dyn Fn(&str)) -> Action {
    log(&format!("-> {}", step.name()));
    let result = exec(&step, facts);
    match &result {
        Ok(msg) => log(&format!("   ok: {msg}")),
        Err(e) => log(&format!("   FAILED: {e}")),
    }
    Action { step, result }
}

// Repair adaptively: run the least disruptive allowed step, re-gather, re-assess, repeat.
pub fn fix_loop(
    gates: &Gates,
    force: bool,
    gather: &mut dyn FnMut() -> Facts,
    exec: &mut StepFn,
    log: &dyn Fn(&str),
) -> (Facts, Vec<Action>) {
    let mut facts = gather();
    let mut done: Vec<Action> = Vec::new();
    if force {
        for step in force_steps(&facts) {
            if let Some(flag) = gates.blocked(&step) {
                log(&format!("-> {} skipped (needs {flag})", step.name()));
                continue;
            }
            done.push(act(step, &facts, exec, log));
            facts = gather();
        }
    }
    for _ in 0..MAX_ROUNDS {
        let tried: Vec<Step> = done.iter().map(|a| a.step.clone()).collect();
        let report = assess(&facts, &tried);
        if report.healthy() {
            break;
        }
        let next = plan(&report, gates).into_iter().find(|(s, b)| b.is_none() && !tried.contains(s));
        let Some((step, _)) = next else {
            break;
        };
        done.push(act(step, &facts, exec, log));
        facts = gather();
    }
    (facts, done)
}

// The machine, the log and the state directory the real steps work with.
pub struct Exec<'a> {
    pub sys: &'a dyn Sys,
    pub log: &'a dyn Fn(&str),
    pub dir: &'a Path,
}

impl Exec<'_> {
    // Run a command and turn a non-zero exit or a networksetup error into Err.
    fn ok(&self, cmd: &[&str], timeout: u64, sudo: bool) -> Result<String, String> {
        let (rc, out) = self.sys.run(cmd, timeout, sudo);
        if rc == 0 && !out.contains("** Error") {
            return Ok(out);
        }
        let first = out.lines().next().unwrap_or_default();
        Err(format!("`{}` exited {rc}: {first}", cmd.join(" ")))
    }

    // Tell whether the Wi-Fi radio reports power on.
    fn wifi_on(&self, dev: &str) -> bool {
        self.sys.run(&["networksetup", "-getairportpower", dev], 15, false).1.trim_end().ends_with("On")
    }

    // Return the current state line of a scutil VPN.
    fn vpn_state(&self, name: &str) -> String {
        parse_vpn_status(&self.sys.run(&["scutil", "--nc", "status", name], 15, false).1).0
    }

    // Poll until a scutil VPN reaches a state or the attempts run out.
    fn wait_vpn(&self, name: &str, want: &str, tries: u32) -> Result<(), String> {
        let mut last = String::new();
        for _ in 0..tries {
            self.sys.pause(2);
            last = self.vpn_state(name);
            if last == want {
                return Ok(());
            }
        }
        Err(last)
    }
}

// Turn Wi-Fi off and on under watchdog protection and wait for an address.
fn cycle_wifi(x: &Exec, dev: &str) -> Result<String, String> {
    let mut watchdog = x.sys.watchdog(x.dir, dev, 45).map_err(|e| format!("watchdog not started: {e}"))?;
    (x.log)(&format!(
        "   watchdog pid {} forces Wi-Fi on in 45s; manual rescue: networksetup -setairportpower {dev} on",
        watchdog.id()
    ));
    x.ok(&["networksetup", "-setairportpower", dev, "off"], 15, false)?;
    x.sys.pause(3);
    let on = x.ok(&["networksetup", "-setairportpower", dev, "on"], 15, false);
    let mut addr = String::new();
    for _ in 0..20 {
        x.sys.pause(2);
        addr = x.sys.run(&["ipconfig", "getifaddr", dev], 15, false).1;
        if addr.matches('.').count() == 3 {
            break;
        }
    }
    if !x.wifi_on(dev) {
        let why = on.err().unwrap_or_default();
        return Err(format!("Wi-Fi did not power back on ({why}); the watchdog retries in 45s"));
    }
    if matches!(watchdog.try_wait(), Ok(None)) {
        let _ = watchdog.kill();
    }
    let _ = watchdog.wait();
    Ok(if addr.is_empty() {
        "radio back on, no address yet".into()
    } else {
        format!("radio back on, address {addr}")
    })
}

// Disable web and secure-web proxies on the service whose server is unreachable.
fn fix_proxy(x: &Exec, svc: &str) -> Result<String, String> {
    let mut disabled = Vec::new();
    let kinds = [
        ("web", "-getwebproxy", "-setwebproxystate"),
        ("secure", "-getsecurewebproxy", "-setsecurewebproxystate"),
    ];
    for (kind, get, set) in kinds {
        let out = x.sys.run(&["networksetup", get, svc], 15, false).1;
        if !out.contains("Enabled: Yes") {
            continue;
        }
        let host = capture(r"Server:\s*(\S+)", &out).unwrap_or_default();
        let port = capture(r"Port:\s*(\d+)", &out).and_then(|p| p.parse().ok()).unwrap_or(0);
        if tcp_host(x.sys, &host, port, 3) {
            continue;
        }
        x.ok(&["networksetup", set, svc, "off"], 15, true)?;
        disabled.push(format!("{kind} proxy {host}:{port} (re-enable: networksetup {set} '{svc}' on)"));
    }
    if disabled.is_empty() {
        return Err(format!(
            "no unreachable proxy is set on service '{svc}' (managed profile or another service); unchanged"
        ));
    }
    Ok(format!("disabled {}", disabled.join(", ")))
}

// Stop a scutil VPN if needed, start it and verify it reports Connected.
fn reconnect_vpn(x: &Exec, name: &str) -> Result<String, String> {
    if x.vpn_state(name) != "Disconnected" {
        x.ok(&["scutil", "--nc", "stop", name], 30, false)?;
        x.wait_vpn(name, "Disconnected", 8)
            .map_err(|s| format!("\"{name}\" did not disconnect (state {s})"))?;
    }
    x.ok(&["scutil", "--nc", "start", name], 30, false)?;
    x.wait_vpn(name, "Connected", 15).map_err(|s| {
        format!("\"{name}\" did not reconnect within 30s (state {s}); it may need sign-in or MFA: connect it from the menu bar")
    })?;
    Ok(format!("\"{name}\" connected"))
}

// Turn the Wi-Fi radio on and confirm it reports On.
fn wifi_on(x: &Exec, dev: &str) -> Result<String, String> {
    x.ok(&["networksetup", "-setairportpower", dev, "on"], 15, false)?;
    x.sys.pause(3);
    if x.wifi_on(dev) {
        return Ok("Wi-Fi on".to_string());
    }
    Err("the radio still reports Off".to_string())
}

// Execute one repair step, checking every command's exit status, then let the network settle.
pub fn exec_step(x: &Exec, step: &Step, f: &Facts) -> Result<String, String> {
    let dev = f.phys.as_ref().map(|p| p.device.clone()).unwrap_or_default();
    let svc = f.phys.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    let gw = f.gateway.clone().unwrap_or_default();
    let result = match step {
        Step::WifiOn => wifi_on(x, &dev),
        Step::FlushDns => x
            .ok(&["dscacheutil", "-flushcache"], 15, true)
            .and_then(|_| x.ok(&["killall", "-HUP", "mDNSResponder"], 15, true))
            .map(|_| "DNS cache flushed".to_string()),
        Step::ClearArp => x.ok(&["arp", "-d", &gw], 15, true).map(|_| format!("ARP entry for {gw} cleared")),
        Step::FixProxy => fix_proxy(x, &svc),
        Step::ReconnectVpn(name) => reconnect_vpn(x, name),
        Step::RenewDhcp => {
            x.ok(&["ipconfig", "set", &dev, "DHCP"], 20, true).map(|_| format!("DHCP renewed on {dev}"))
        }
        Step::CycleWifi => cycle_wifi(x, &dev),
        Step::FlushRoutes => {
            x.ok(&["route", "-n", "flush"], 15, true).map(|_| "routing table flushed".to_string())
        }
    };
    x.sys.pause(4);
    result
}

// Tests for gating, planning, the fix loop and every step against a scripted Mac.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::assess::tests::{healthy, internal, with_vpn};
    use crate::facts::tests::mac;
    use crate::sys::fake::Fake;

    // Run one step against a scripted Mac and return the result and the commands run.
    fn step(sys: &Fake, s: Step) -> Result<String, String> {
        let dir = std::env::temp_dir();
        let log = |_: &str| {};
        exec_step(&Exec { sys, log: &log, dir: &dir }, &s, &healthy())
    }

    // Check that gated steps need their flag and disruptive steps need --allow-remote over SSH.
    #[test]
    fn test_gates_blocked() {
        let g = Gates::default();
        assert_eq!(g.blocked(&Step::ReconnectVpn("W".into())), Some("--vpn"));
        assert_eq!(g.blocked(&Step::FixProxy), Some("--fix-proxy"));
        assert_eq!(g.blocked(&Step::FlushRoutes), Some("--allow-route-flush"));
        assert_eq!(g.blocked(&Step::FlushDns), None);
        let all = Gates { vpn: true, fix_proxy: true, allow_route_flush: true, ..Gates::default() };
        assert_eq!(all.blocked(&Step::FixProxy), None);
        assert_eq!(all.blocked(&Step::FlushRoutes), None);
        let ssh = Gates { over_ssh: true, ..all.clone() };
        assert_eq!(ssh.blocked(&Step::ReconnectVpn("W".into())), Some("--allow-remote"));
        assert_eq!(ssh.blocked(&Step::CycleWifi), Some("--allow-remote"));
        assert_eq!(ssh.blocked(&Step::FlushDns), None);
        assert_eq!(Gates { allow_remote: true, ..ssh }.blocked(&Step::CycleWifi), None);
    }

    // Check that plans dedupe, sort by disruption and skip manual remedies.
    #[test]
    fn test_plan_dedupes_and_sorts() {
        let finding = |remedy| crate::assess::Finding { code: "x", summary: String::new(), remedy };
        let report = Report {
            checks: vec![],
            findings: vec![
                finding(Remedy::Steps(vec![Step::CycleWifi, Step::FlushDns])),
                finding(Remedy::Manual("call the ISP".into())),
                finding(Remedy::Steps(vec![Step::FlushDns, Step::FixProxy])),
            ],
        };
        let p = plan(&report, &Gates::default());
        assert_eq!(
            p,
            vec![(Step::FlushDns, None), (Step::FixProxy, Some("--fix-proxy")), (Step::CycleWifi, None)]
        );
    }

    // Check the --force ladder for Wi-Fi, wired-manual and interface-less snapshots.
    #[test]
    fn test_force_steps_variants() {
        assert_eq!(
            force_steps(&healthy()),
            vec![Step::FlushDns, Step::ClearArp, Step::RenewDhcp, Step::CycleWifi]
        );
        let mut wired = healthy();
        wired.phys.as_mut().unwrap().port = "Ethernet".into();
        wired.dhcp = false;
        wired.gateway = None;
        assert_eq!(force_steps(&wired), vec![Step::FlushDns]);
        let mut none = healthy();
        none.phys = None;
        assert_eq!(force_steps(&none), vec![Step::FlushDns, Step::ClearArp]);
    }

    // Check that a healthy network runs no steps without --force.
    #[test]
    fn test_fix_loop_healthy_noop() {
        let mut ran = Vec::new();
        let (_, done) = fix_loop(
            &Gates::default(),
            false,
            &mut healthy,
            &mut |s, _| {
                ran.push(s.clone());
                Ok(String::new())
            },
            &|_| {},
        );
        assert!(done.is_empty() && ran.is_empty());
    }

    // Check that --force runs the full ladder even when healthy, and skips what SSH blocks.
    #[test]
    fn test_fix_loop_force_runs_ladder() {
        let (_, done) =
            fix_loop(&Gates::default(), true, &mut healthy, &mut |_, _| Ok("done".into()), &|_| {});
        let steps: Vec<Step> = done.into_iter().map(|a| a.step).collect();
        assert_eq!(steps, vec![Step::FlushDns, Step::ClearArp, Step::RenewDhcp, Step::CycleWifi]);
        let lines = RefCell::new(Vec::new());
        let ssh = Gates { over_ssh: true, ..Gates::default() };
        let log = |m: &str| lines.borrow_mut().push(m.to_string());
        let (_, done) = fix_loop(&ssh, true, &mut healthy, &mut |_, _| Ok(String::new()), &log);
        assert_eq!(done.len(), 2);
        assert!(lines.borrow().iter().any(|l| l.contains("skipped (needs --allow-remote)")));
    }

    // Check that a VPN-only fault reconnects the VPN and never cycles Wi-Fi.
    #[test]
    fn test_fix_loop_vpn_fault_spares_wifi() {
        let fixed = RefCell::new(false);
        let mut gather = || {
            let mut f = with_vpn(healthy());
            f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "utun4", Some(*fixed.borrow()))];
            f
        };
        let gates = Gates { vpn: true, ..Gates::default() };
        let (facts, done) = fix_loop(
            &gates,
            false,
            &mut gather,
            &mut |s, _| {
                *fixed.borrow_mut() = matches!(s, Step::ReconnectVpn(_));
                Ok(String::new())
            },
            &|_| {},
        );
        let steps: Vec<Step> = done.into_iter().map(|a| a.step).collect();
        assert_eq!(steps, vec![Step::ReconnectVpn("Work".into())]);
        assert!(assess(&facts, &steps).healthy());
    }

    // Check that without --vpn a VPN fault runs nothing and reports the flag.
    #[test]
    fn test_fix_loop_vpn_needs_flag() {
        let mut gather = || {
            let mut f = with_vpn(healthy());
            f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "utun4", Some(false))];
            f
        };
        let (facts, done) =
            fix_loop(&Gates::default(), false, &mut gather, &mut |_, _| Ok(String::new()), &|_| {});
        assert!(done.is_empty());
        let p = plan(&assess(&facts, &[]), &Gates::default());
        assert_eq!(p, vec![(Step::ReconnectVpn("Work".into()), Some("--vpn"))]);
    }

    // Check that a failed step is logged as failed and the ladder moves on.
    #[test]
    fn test_fix_loop_failed_step_recorded() {
        let mut gather = || {
            let mut f = healthy();
            f.link.ipv4 = None;
            f
        };
        let lines = RefCell::new(Vec::new());
        let log = |m: &str| lines.borrow_mut().push(m.to_string());
        let (_, done) = fix_loop(
            &Gates::default(),
            false,
            &mut gather,
            &mut |s, _| match s {
                Step::RenewDhcp => Err("sudo: a password is required".into()),
                _ => Ok("ok".into()),
            },
            &log,
        );
        assert_eq!(done.len(), 2);
        assert!(done[0].result.is_err());
        assert_eq!(done[1].step, Step::CycleWifi);
        assert!(lines.borrow().iter().any(|l| l.contains("FAILED: sudo")));
    }

    // Check the simple privileged steps succeed and report failures with the command.
    #[test]
    fn test_exec_simple_steps() {
        let sys = mac();
        sys.on("dscacheutil -flushcache", &[(0, "")])
            .on("killall -HUP mDNSResponder", &[(1, "No matching processes")]);
        assert!(step(&sys, Step::FlushDns).unwrap_err().contains("killall -HUP mDNSResponder` exited 1"));
        sys.on("killall -HUP mDNSResponder", &[(0, "")]);
        assert_eq!(step(&sys, Step::FlushDns).unwrap(), "DNS cache flushed");
        sys.on("dscacheutil -flushcache", &[(1, "sudo: a password is required")]);
        assert!(step(&sys, Step::FlushDns).is_err());
        assert_eq!(sys.count("sudo killall -HUP mDNSResponder"), 2);
        sys.on("arp -d 192.168.1.1", &[(0, "")])
            .on("ipconfig set en0 DHCP", &[(0, "")])
            .on("route -n flush", &[(0, "")]);
        assert!(step(&sys, Step::ClearArp).unwrap().contains("192.168.1.1"));
        assert!(step(&sys, Step::RenewDhcp).unwrap().contains("en0"));
        assert!(step(&sys, Step::FlushRoutes).is_ok());
        sys.on("ipconfig set en0 DHCP", &[(1, "")]);
        assert!(step(&sys, Step::RenewDhcp).is_err());
    }

    // Check turning Wi-Fi on: success, a radio that stays off, and networksetup error text.
    #[test]
    fn test_exec_wifi_on() {
        let sys = mac();
        sys.on("networksetup -setairportpower en0 on", &[(0, "")]);
        assert_eq!(step(&sys, Step::WifiOn).unwrap(), "Wi-Fi on");
        sys.on("networksetup -getairportpower en0", &[(0, "Wi-Fi Power (en0): Off")]);
        assert_eq!(step(&sys, Step::WifiOn).unwrap_err(), "the radio still reports Off");
        sys.on(
            "networksetup -setairportpower en0 on",
            &[(0, "** Error: Unable to find item in network database.")],
        );
        assert!(step(&sys, Step::WifiOn).unwrap_err().contains("exited 0: ** Error"));
    }

    // Check the Wi-Fi cycle: address found, no address, radio stuck off, off failing, watchdog failing.
    #[test]
    fn test_exec_cycle_wifi() {
        let sys = mac();
        sys.on("networksetup -setairportpower en0 off", &[(0, "")])
            .on("networksetup -setairportpower en0 on", &[(0, "")])
            .on("ipconfig getifaddr en0", &[(0, ""), (0, "192.168.1.5")]);
        assert_eq!(step(&sys, Step::CycleWifi).unwrap(), "radio back on, address 192.168.1.5");
        assert_eq!(sys.count("watchdog en0"), 1);
        sys.on("ipconfig getifaddr en0", &[(1, "")]);
        assert_eq!(step(&sys, Step::CycleWifi).unwrap(), "radio back on, no address yet");
        sys.on("networksetup -getairportpower en0", &[(0, "Wi-Fi Power (en0): Off")])
            .on("networksetup -setairportpower en0 on", &[(1, "failed")]);
        assert!(step(&sys, Step::CycleWifi).unwrap_err().contains("did not power back on"));
        sys.on("networksetup -setairportpower en0 off", &[(1, "")]);
        assert!(step(&sys, Step::CycleWifi).unwrap_err().contains("setairportpower en0 off"));
        let broken = Fake { watchdog_fails: true, ..mac() };
        assert!(step(&broken, Step::CycleWifi).unwrap_err().starts_with("watchdog not started"));
        assert_eq!(broken.count("networksetup -setairportpower en0 off"), 0);
        let done = Fake { watchdog_done: true, ..mac() };
        done.on("networksetup -setairportpower en0 off", &[(0, "")])
            .on("networksetup -setairportpower en0 on", &[(0, "")]);
        done.on("ipconfig getifaddr en0", &[(0, "192.168.1.5")]);
        assert!(step(&done, Step::CycleWifi).is_ok());
    }

    // Check disabling proxies: unreachable disabled, reachable kept, nothing to do, and sudo failing.
    #[test]
    fn test_exec_fix_proxy() {
        let sys = mac();
        sys.on("networksetup -getwebproxy Wi-Fi", &[(0, "Enabled: Yes\nServer: dead.corp\nPort: 8080")])
            .on(
                "networksetup -getsecurewebproxy Wi-Fi",
                &[(0, "Enabled: Yes\nServer: live.corp\nPort: 8443")],
            )
            .on("networksetup -setwebproxystate Wi-Fi off", &[(0, "")])
            .name("live.corp", &["10.9.9.9"])
            .listen("10.9.9.9", 8443);
        let ok = step(&sys, Step::FixProxy).unwrap();
        assert!(ok.contains("web proxy dead.corp:8080") && !ok.contains("live.corp"));
        assert_eq!(sys.count("sudo networksetup -setwebproxystate Wi-Fi off"), 1);
        sys.on("networksetup -setwebproxystate Wi-Fi off", &[(1, "")]);
        assert!(step(&sys, Step::FixProxy).unwrap_err().contains("-setwebproxystate"));
        sys.on("networksetup -getwebproxy Wi-Fi", &[(0, "Enabled: No")]);
        assert!(step(&sys, Step::FixProxy).unwrap_err().contains("no unreachable proxy"));
    }

    // Check reconnecting a connected VPN through stop and start.
    #[test]
    fn test_exec_vpn_reconnect_success() {
        let sys = mac();
        sys.on(
            "scutil --nc status Work",
            &[(0, "Connected"), (0, "Disconnected"), (0, "Connecting"), (0, "Connected")],
        )
        .on("scutil --nc stop Work", &[(0, "")])
        .on("scutil --nc start Work", &[(0, "")]);
        assert_eq!(step(&sys, Step::ReconnectVpn("Work".into())).unwrap(), "\"Work\" connected");
    }

    // Check VPN reconnect failures: never connects, never disconnects, stop or start failing.
    #[test]
    fn test_exec_vpn_reconnect_failures() {
        let sys = mac();
        sys.on("scutil --nc status Work", &[(0, "Disconnected")]).on("scutil --nc start Work", &[(0, "")]);
        let err = step(&sys, Step::ReconnectVpn("Work".into())).unwrap_err();
        assert!(err.contains("MFA") && err.contains("state Disconnected"));
        assert_eq!(sys.count("scutil --nc stop Work"), 0);
        sys.on("scutil --nc start Work", &[(1, "No service")]);
        assert!(step(&sys, Step::ReconnectVpn("Work".into())).unwrap_err().contains("start Work"));
        sys.on("scutil --nc status Work", &[(0, "Connected")]).on("scutil --nc stop Work", &[(0, "")]);
        assert!(step(&sys, Step::ReconnectVpn("Work".into())).unwrap_err().contains("did not disconnect"));
        sys.on("scutil --nc stop Work", &[(1, "")]);
        assert!(step(&sys, Step::ReconnectVpn("Work".into())).unwrap_err().contains("stop Work"));
    }
}
