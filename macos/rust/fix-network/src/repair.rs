// Escalating repair steps, Wi-Fi watchdog and VPN toggling for the fix command.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use super::{c, capture, health, sh, sh_opts, shell_quote, sleep, tcp, FixArgs, Health, Info, Session};

// Write recover.sh and start a detached watchdog that turns Wi-Fi back on.
fn stage_recovery(s: &Session, dev: &str, delay: u64) -> std::io::Result<Child> {
    let path = s.dir.join("recover.sh");
    let log = shell_quote(&s.log_path.to_string_lossy());
    let script = format!(
        "#!/bin/sh\n# Re-enable Wi-Fi no matter what. Log: {}\nsleep {delay}\n\
         /usr/sbin/networksetup -setairportpower {} on\n\
         echo \"$(date) watchdog ensured Wi-Fi on\" >> {log}\n",
        s.log_path.display(),
        shell_quote(dev)
    );
    fs::write(&path, script)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    let child = Command::new("/bin/sh")
        .arg(&path)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    s.log(&format!(
        "recovery watchdog staged (pid {}): Wi-Fi forced on in {delay}s. \
         Manual: networksetup -setairportpower {dev} on",
        child.id()
    ));
    Ok(child)
}

// Turn Wi-Fi off and on under watchdog protection and wait for an address.
fn cycle_wifi(s: &mut Session, dev: &str) -> Result<(), String> {
    let mut watchdog = stage_recovery(s, dev, 45).map_err(|e| e.to_string())?;
    s.log("  cycling Wi-Fi radio ...");
    sh(&["networksetup", "-setairportpower", dev, "off"]);
    sleep(3);
    sh(&["networksetup", "-setairportpower", dev, "on"]);
    s.changes.push(format!("cycled Wi-Fi radio on {dev}"));
    for _ in 0..20 {
        sleep(2);
        if sh(&["ipconfig", "getifaddr", dev]).1.matches('.').count() == 3 {
            break;
        }
    }
    let powered = sh(&["networksetup", "-getairportpower", dev]).1.ends_with("On");
    if matches!(watchdog.try_wait(), Ok(None)) && powered {
        let _ = watchdog.kill();
    }
    Ok(())
}

// Disable system proxies whose server is unreachable.
fn stale_proxy(s: &Session, svc: &str) {
    for (kind, get, setter) in [
        ("web", "-getwebproxy", "-setwebproxystate"),
        ("secure", "-getsecurewebproxy", "-setsecurewebproxystate"),
    ] {
        let out = sh(&["networksetup", get, svc]).1;
        if !out.contains("Enabled: Yes") {
            continue;
        }
        let host = capture(r"Server:\s*(\S+)", &out).unwrap_or_default();
        let port = capture(r"Port:\s*(\d+)", &out).and_then(|p| p.parse().ok()).unwrap_or(0);
        if tcp(&host, port, 3) {
            continue;
        }
        s.log(&format!(
            "   {kind} proxy {host}:{port} unreachable; disabling (original recorded; \
             re-enable: networksetup {setter} '{svc}' on)"
        ));
        sh(&["networksetup", setter, svc, "off"]);
    }
}

// One repair action in the escalation ladder.
#[derive(Clone, Copy)]
enum Step {
    FlushDns,
    ClearArp,
    RenewDhcp,
    CycleWifi,
    FixProxy,
    FlushRoutes,
}

impl Step {
    // Return the step's display name.
    fn name(self) -> &'static str {
        match self {
            Step::FlushDns => "flush DNS cache",
            Step::ClearArp => "clear ARP entry for gateway",
            Step::RenewDhcp => "renew DHCP lease",
            Step::CycleWifi => "cycle Wi-Fi (watchdog-protected)",
            Step::FixProxy => "disable unreachable proxies",
            Step::FlushRoutes => "route -n flush (broad)",
        }
    }
}

// Build the repair steps that fit the failing layer and flags.
fn plan_steps(s: &Session, h: &Health, info: &Info, args: &FixArgs) -> Vec<Step> {
    let v = h.verdict;
    let force = args.force;
    let dhcp = s.original_text("getinfo").contains("DHCP Configuration");
    let mut steps = Vec::new();
    if force || ["dns", "https-proxy-or-tls", "upstream-or-routing"].contains(&v) {
        steps.push(Step::FlushDns);
    }
    if !h.gateway.is_empty() && (force || ["gateway-unreachable", "upstream-or-routing"].contains(&v)) {
        steps.push(Step::ClearArp);
    }
    let lease_layers = ["no-dhcp-lease", "gateway-unreachable", "captive-portal", "upstream-or-routing"];
    if info.wifi_device.is_some() && dhcp && (force || lease_layers.contains(&v)) {
        steps.push(Step::RenewDhcp);
    }
    if info.wifi_device.is_some() {
        steps.push(Step::CycleWifi);
    }
    if args.fix_proxy && info.wifi_service.is_some() {
        steps.push(Step::FixProxy);
    }
    if args.allow_route_flush {
        steps.push(Step::FlushRoutes);
    }
    steps
}

// Execute one repair step.
fn run_step(s: &mut Session, step: Step, info: &Info, gateway: &str) -> Result<(), String> {
    let dev = info.wifi_device.as_deref().unwrap_or_default();
    match step {
        Step::FlushDns => {
            sh_opts(&["dscacheutil", "-flushcache"], 15, true);
            sh_opts(&["killall", "-HUP", "mDNSResponder"], 15, true);
        }
        Step::ClearArp => {
            sh_opts(&["arp", "-d", gateway], 15, true);
        }
        Step::RenewDhcp => {
            sh_opts(&["ipconfig", "set", dev, "DHCP"], 20, false);
        }
        Step::CycleWifi => return cycle_wifi(s, dev),
        Step::FixProxy => stale_proxy(s, info.wifi_service.as_deref().unwrap_or_default()),
        Step::FlushRoutes => {
            sh_opts(&["route", "-n", "flush"], 15, true);
        }
    }
    Ok(())
}

// Disconnect connected VPNs and return their names.
fn vpns_off(s: &Session, info: &Info) -> Vec<String> {
    let mut down = Vec::new();
    for (name, _) in info.vpns.iter().filter(|(_, up)| *up) {
        s.log(&c("36", &format!("-> disconnecting VPN {name} to test plain internet")));
        sh_opts(&["scutil", "--nc", "stop", name], 30, false);
        down.push(name.clone());
    }
    down
}

// Reconnect VPNs taken down earlier and wait for each to connect.
fn vpns_on(s: &Session, names: &[String]) {
    for name in names {
        s.log(&c("36", &format!("-> reconnecting VPN {name}")));
        sh_opts(&["scutil", "--nc", "start", name], 30, false);
        for _ in 0..15 {
            sleep(2);
            let status = sh(&["scutil", "--nc", "status", name]).1;
            if status.lines().next().unwrap_or_default().trim() == "Connected" {
                break;
            }
        }
    }
}

// Return advice for a layer that is still failing.
fn advice(verdict: &str) -> Option<&'static str> {
    match verdict {
        "captive-portal" => Some("Open a browser and complete the Wi-Fi sign-in page."),
        "upstream-or-routing" => {
            Some("Gateway answers but the internet does not: likely ISP/router outage - check the router.")
        }
        "gateway-unreachable" => Some(
            "Router unreachable: check signal/AP; consider restarting the router (not done automatically).",
        ),
        "no-dhcp-lease" => Some("No lease: the router's DHCP may be down or the Wi-Fi password changed."),
        _ => None,
    }
}

// Apply repair steps from the failing layer upward, retesting after each.
pub fn fix(s: &mut Session, info: &Info, args: &FixArgs) -> Health {
    let mut h = health(s, info);
    if h.verdict == "healthy" && !args.force {
        s.log(&c("32", "Internet already healthy; no repair needed (use --force to run steps anyway)."));
        return h;
    }
    let gateway = h.gateway.clone();
    let mut vpns_down: Vec<String> = Vec::new();
    for step in plan_steps(s, &h, info, args) {
        if h.verdict == "healthy" {
            break;
        }
        s.log(&c("36", &format!("-> {}", step.name())));
        if let Err(e) = run_step(s, step, info, &gateway) {
            s.log(&format!("   step failed: {e}"));
        }
        s.changes.push(step.name().to_string());
        sleep(4);
        h = health(s, info);
        // A VPN may be the cause: bring it down only after a cheaper step fails, once.
        let vpn_up = info.vpns.iter().any(|(_, up)| *up);
        if h.verdict != "healthy" && vpns_down.is_empty() && vpn_up && args.vpn {
            vpns_down = vpns_off(s, info);
            sleep(5);
            h = health(s, info);
        }
    }
    if h.verdict == "healthy" {
        s.log(&c("32", "Internet restored."));
    } else {
        s.log(&c("31", &format!("Still failing at layer: {}", h.verdict)));
        if let Some(text) = advice(h.verdict) {
            s.log(&format!("  {text}"));
        }
    }
    vpns_on(s, &vpns_down);
    h
}
