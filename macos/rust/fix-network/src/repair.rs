// Repair planning, the adaptive fix loop, the steps themselves and the Wi-Fi watchdog.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::assess::{assess, Remedy, Report, Step};
use crate::facts::{tcp, Facts};
use crate::parse::parse_vpn_status;
use crate::run::{capture, shell_quote};

// Executes one repair step against a snapshot.
pub type StepFn<'a> = dyn FnMut(&Step, &Facts) -> Result<String, String> + 'a;

// Runs a command line with a timeout and optional sudo, returning (exit code, output).
pub type RunFn<'a> = dyn Fn(&[&str], u64, bool) -> (i32, String) + 'a;

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

// Commands, waits and logging used by the real steps; faked in tests.
pub struct Exec<'a> {
    pub run: &'a RunFn<'a>,
    pub pause: &'a dyn Fn(u64),
    pub log: &'a dyn Fn(&str),
    pub state_dir: &'a Path,
}

impl Exec<'_> {
    // Run a command and turn a non-zero exit or a networksetup error into Err.
    fn ok(&self, cmd: &[&str], timeout: u64, sudo: bool) -> Result<String, String> {
        let (rc, out) = (self.run)(cmd, timeout, sudo);
        if rc == 0 && !out.contains("** Error") {
            return Ok(out);
        }
        let first = out.lines().next().unwrap_or_default();
        Err(format!("`{}` exited {rc}: {first}", cmd.join(" ")))
    }

    // Tell whether the Wi-Fi radio reports power on.
    fn wifi_on(&self, dev: &str) -> bool {
        (self.run)(&["networksetup", "-getairportpower", dev], 15, false).1.trim_end().ends_with("On")
    }

    // Return the current state line of a scutil VPN.
    fn vpn_state(&self, name: &str) -> String {
        parse_vpn_status(&(self.run)(&["scutil", "--nc", "status", name], 15, false).1).0
    }

    // Poll until a scutil VPN reaches a state or the attempts run out.
    fn wait_vpn(&self, name: &str, want: &str, tries: u32) -> Result<(), String> {
        let mut last = String::new();
        for _ in 0..tries {
            (self.pause)(2);
            last = self.vpn_state(name);
            if last == want {
                return Ok(());
            }
        }
        Err(last)
    }
}

// Write recover.sh and start a detached watchdog that turns Wi-Fi back on.
fn stage_recovery(dir: &Path, dev: &str, delay: u64) -> std::io::Result<Child> {
    let path = dir.join("recover.sh");
    let log = dir.join("diagnostic.log");
    let script = format!(
        "#!/bin/sh\n# Re-enable Wi-Fi no matter what. Log: {}\nsleep {delay}\n\
         /usr/sbin/networksetup -setairportpower {} on\n\
         echo \"$(date) watchdog ensured Wi-Fi on\" >> {}\n",
        log.display(),
        shell_quote(dev),
        shell_quote(&log.to_string_lossy())
    );
    fs::write(&path, script)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Command::new("/bin/sh")
        .arg(&path)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

// Turn Wi-Fi off and on under watchdog protection and wait for an address.
fn cycle_wifi(x: &Exec, dev: &str) -> Result<String, String> {
    let mut watchdog =
        stage_recovery(x.state_dir, dev, 45).map_err(|e| format!("watchdog not started: {e}"))?;
    (x.log)(&format!(
        "   watchdog pid {} forces Wi-Fi on in 45s; manual rescue: networksetup -setairportpower {dev} on",
        watchdog.id()
    ));
    x.ok(&["networksetup", "-setairportpower", dev, "off"], 15, false)?;
    (x.pause)(3);
    let on = x.ok(&["networksetup", "-setairportpower", dev, "on"], 15, false);
    let mut addr = String::new();
    for _ in 0..20 {
        (x.pause)(2);
        addr = (x.run)(&["ipconfig", "getifaddr", dev], 15, false).1;
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
    Ok(if addr.is_empty() {
        "radio back on, no address yet".into()
    } else {
        format!("radio back on, address {addr}")
    })
}

// Disable web and secure-web proxies on the service whose server is unreachable.
fn fix_proxy(x: &Exec, svc: &str) -> Result<String, String> {
    let mut disabled = Vec::new();
    for (kind, get, set) in [
        ("web", "-getwebproxy", "-setwebproxystate"),
        ("secure", "-getsecurewebproxy", "-setsecurewebproxystate"),
    ] {
        let out = (x.run)(&["networksetup", get, svc], 15, false).1;
        if !out.contains("Enabled: Yes") {
            continue;
        }
        let host = capture(r"Server:\s*(\S+)", &out).unwrap_or_default();
        let port = capture(r"Port:\s*(\d+)", &out).and_then(|p| p.parse().ok()).unwrap_or(0);
        if tcp(&host, port, 3) {
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

// Execute one repair step, checking every command's exit status, then let the network settle.
pub fn exec_step(x: &Exec, step: &Step, f: &Facts) -> Result<String, String> {
    let dev = f.phys.as_ref().map(|p| p.device.clone()).unwrap_or_default();
    let svc = f.phys.as_ref().map(|p| p.name.clone()).unwrap_or_default();
    let result = match step {
        Step::WifiOn => x.ok(&["networksetup", "-setairportpower", &dev, "on"], 15, false).and_then(|_| {
            (x.pause)(3);
            if x.wifi_on(&dev) {
                Ok("Wi-Fi on".to_string())
            } else {
                Err("the radio still reports Off".to_string())
            }
        }),
        Step::FlushDns => x
            .ok(&["dscacheutil", "-flushcache"], 15, true)
            .and_then(|_| x.ok(&["killall", "-HUP", "mDNSResponder"], 15, true))
            .map(|_| "DNS cache flushed".to_string()),
        Step::ClearArp => {
            let gw = f.gateway.clone().unwrap_or_default();
            x.ok(&["arp", "-d", &gw], 15, true).map(|_| format!("ARP entry for {gw} cleared"))
        }
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
    (x.pause)(4);
    result
}

// Tests for gating, planning, the fix loop and step execution with fake commands.
#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::assess::tests::{healthy, internal, with_vpn};

    // Build a fake executor whose commands answer from a closure and are recorded.
    fn with_exec<R>(
        answer: &dyn Fn(&[&str]) -> (i32, String),
        body: impl FnOnce(&Exec) -> R,
    ) -> (R, Vec<String>) {
        let calls = RefCell::new(Vec::new());
        let run = |cmd: &[&str], _t: u64, sudo: bool| {
            calls.borrow_mut().push(format!("{}{}", if sudo { "sudo " } else { "" }, cmd.join(" ")));
            answer(cmd)
        };
        let pause = |_s: u64| {};
        let log = |_m: &str| {};
        let dir = std::env::temp_dir();
        let x = Exec { run: &run, pause: &pause, log: &log, state_dir: &dir };
        let out = body(&x);
        (out, calls.into_inner())
    }

    // Check that gated steps need their flag and disruptive steps need --allow-remote over SSH.
    #[test]
    fn test_gates_blocked() {
        let g = Gates::default();
        assert_eq!(g.blocked(&Step::ReconnectVpn("W".into())), Some("--vpn"));
        assert_eq!(g.blocked(&Step::FlushDns), None);
        let ssh = Gates { over_ssh: true, vpn: true, ..Gates::default() };
        assert_eq!(ssh.blocked(&Step::ReconnectVpn("W".into())), Some("--allow-remote"));
        assert_eq!(ssh.blocked(&Step::CycleWifi), Some("--allow-remote"));
        assert_eq!(ssh.blocked(&Step::FlushDns), None);
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

    // Check that --force runs the full ladder even when healthy.
    #[test]
    fn test_fix_loop_force_runs_ladder() {
        let (_, done) =
            fix_loop(&Gates::default(), true, &mut healthy, &mut |_, _| Ok(String::new()), &|_| {});
        let steps: Vec<Step> = done.into_iter().map(|a| a.step).collect();
        assert_eq!(steps, vec![Step::FlushDns, Step::ClearArp, Step::RenewDhcp, Step::CycleWifi]);
    }

    // Check that a VPN-only fault reconnects the VPN and never cycles Wi-Fi.
    #[test]
    fn test_fix_loop_vpn_fault_spares_wifi() {
        let fixed = RefCell::new(false);
        let mut gather = || {
            let mut f = with_vpn(healthy());
            let ok = *fixed.borrow();
            f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "utun4", Some(ok))];
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

    // Check that a failed step is recorded as failed and the ladder moves on.
    #[test]
    fn test_fix_loop_failed_step_recorded() {
        let mut gather = || {
            let mut f = healthy();
            f.link.ipv4 = None;
            f
        };
        let (_, done) = fix_loop(
            &Gates::default(),
            false,
            &mut gather,
            &mut |s, _| match s {
                Step::RenewDhcp => Err("sudo: a password is required".into()),
                _ => Ok(String::new()),
            },
            &|_| {},
        );
        assert_eq!(done.len(), 2);
        assert!(done[0].result.is_err());
        assert_eq!(done[1].step, Step::CycleWifi);
    }

    // Check that a failing privileged command fails the step.
    #[test]
    fn test_exec_step_privileged_failure() {
        let answer = |cmd: &[&str]| {
            if cmd[0] == "killall" {
                (1, "sudo: a password is required".into())
            } else {
                (0, String::new())
            }
        };
        let (r, calls) = with_exec(&answer, |x| exec_step(x, &Step::FlushDns, &healthy()));
        assert!(r.unwrap_err().contains("killall -HUP mDNSResponder` exited 1"));
        assert_eq!(calls, vec!["sudo dscacheutil -flushcache", "sudo killall -HUP mDNSResponder"]);
    }

    // Check that networksetup errors printed with exit 0 still count as failures.
    #[test]
    fn test_exec_step_networksetup_error_text() {
        let answer = |_: &[&str]| (0, "** Error: Unable to find item in network database.".to_string());
        let (r, _) = with_exec(&answer, |x| exec_step(x, &Step::WifiOn, &healthy()));
        assert!(r.is_err());
    }

    // Check that a VPN that never reaches Connected fails the reconnect with an MFA hint.
    #[test]
    fn test_exec_step_vpn_reconnect_failure() {
        let answer = |cmd: &[&str]| match cmd.get(2) {
            Some(&"status") => (0, "Disconnected".to_string()),
            _ => (0, String::new()),
        };
        let (r, calls) = with_exec(&answer, |x| exec_step(x, &Step::ReconnectVpn("Work".into()), &healthy()));
        assert!(r.unwrap_err().contains("MFA"));
        assert!(calls.contains(&"scutil --nc start Work".to_string()));
        assert!(!calls.contains(&"scutil --nc stop Work".to_string()));
    }

    // Check a successful stop-then-start reconnect.
    #[test]
    fn test_exec_step_vpn_reconnect_success() {
        let state = RefCell::new("Connected");
        let answer = |cmd: &[&str]| {
            match cmd.get(2) {
                Some(&"stop") => *state.borrow_mut() = "Disconnected",
                Some(&"start") => *state.borrow_mut() = "Connected",
                _ => {}
            }
            (0, state.borrow().to_string())
        };
        let (r, _) = with_exec(&answer, |x| exec_step(x, &Step::ReconnectVpn("Work".into()), &healthy()));
        assert_eq!(r.unwrap(), "\"Work\" connected");
    }
}
