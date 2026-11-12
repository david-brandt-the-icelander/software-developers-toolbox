// Text and JSON rendering of checks, findings, plans and actions.
use serde_json::{json, Value};

use crate::assess::{Remedy, Report, State, Step};
use crate::facts::Facts;
use crate::repair::{Action, Gates};
use crate::run::{c, Session};

// Color a padded state label for the terminal.
fn colored(state: State) -> String {
    let code = match state {
        State::Healthy => "32",
        State::Failed => "31",
        _ => "33",
    };
    c(code, &format!("{:<11}", state.label()))
}

// Return the distinct flags that block any of the steps.
fn needed_flags(steps: &[Step], gates: &Gates) -> Vec<&'static str> {
    let mut flags: Vec<&'static str> = Vec::new();
    for flag in steps.iter().filter_map(|s| gates.blocked(s)) {
        if !flags.contains(&flag) {
            flags.push(flag);
        }
    }
    flags
}

// Describe a remedy; in fix mode the gates say which flags were missing.
fn remedy_text(r: &Remedy, f: &Facts, gates: Option<&Gates>) -> String {
    match r {
        Remedy::Steps(steps) => {
            let names: Vec<String> = steps.iter().map(Step::name).collect();
            let names = names.join(", then ");
            match gates {
                None => {
                    let g = Gates { over_ssh: f.over_ssh, ..Gates::default() };
                    let flags: String = needed_flags(steps, &g).iter().map(|x| format!(" {x}")).collect();
                    let hosts = if f.internal.is_empty() { "" } else { " (repeat your --internal-host)" };
                    format!("fixable: {names}  ->  fix-network fix{flags}{hosts}")
                }
                Some(g) => match needed_flags(steps, g).as_slice() {
                    [] => format!("fixable but not run: {names}"),
                    flags => format!("fixable but not allowed: {names} (add {})", flags.join(" ")),
                },
            }
        }
        Remedy::Manual(text) => format!("not fixable by this tool: {text}"),
        Remedy::Reboot(text) => format!("reboot needed: {text}"),
    }
}

// Print the extra details shown with --verbose.
fn print_details(s: &Session, f: &Facts) {
    s.log(&c("1", "Details"));
    let dev = f.phys.as_ref().map_or("-".into(), |p| format!("{} ({}, {})", p.device, p.name, p.port));
    s.log(&format!("  macOS {} | interface {dev} | over SSH: {}", f.macos, f.over_ssh));
    s.log(&format!(
        "  default route {} | route to internet {}",
        f.default_if.as_deref().unwrap_or("-"),
        f.public_if.as_deref().unwrap_or("-")
    ));
    if let Some(age) = f.wake_age {
        s.log(&format!("  last wake from sleep {} min ago", age / 60));
    }
    for t in f.tunnels() {
        let n = f.routes.iter().filter(|r| r.netif == t && !r.link_local()).count();
        s.log(&format!("  tunnel {t}: {n} routes"));
    }
    for v in &f.vpns {
        s.log(&format!("  scutil VPN \"{}\": {} {}", v.name, v.state, v.iface.as_deref().unwrap_or("")));
    }
    for r in f.resolvers.iter().filter(|r| r.supplemental()) {
        let reach = if r.reachable == Some(false) { "not reachable" } else { "reachable" };
        s.log(&format!(
            "  split DNS {} -> {} via {} ({reach})",
            r.domain.as_deref().unwrap_or("?"),
            r.nameservers.join(", "),
            r.iface.as_deref().unwrap_or("?")
        ));
    }
    for r in f.stale_routes() {
        s.log(&format!("  stale route {} -> {} via {}", r.dest, r.gateway, r.netif));
    }
}

// Print the checks table, findings with remedies and the Wi-Fi reset line.
pub fn print_report(s: &Session, r: &Report, f: &Facts, gates: Option<&Gates>, verbose: bool) {
    s.log(&c("1", "Checks"));
    for ch in &r.checks {
        s.log(&format!("  {:<18} {} {}", ch.name, colored(ch.state), ch.detail));
    }
    if verbose {
        print_details(s, f);
    }
    s.log(&c("1", "Diagnosis"));
    if r.findings.is_empty() {
        s.log("  no problems found");
    }
    for (n, finding) in r.findings.iter().enumerate() {
        s.log(&format!("  {}. {}", n + 1, finding.summary));
        s.log(&format!("     {}", remedy_text(&finding.remedy, f, gates)));
    }
    let untested: Vec<&str> =
        r.checks.iter().filter(|c| c.state == State::NotTested).map(|c| c.name).collect();
    if !untested.is_empty() {
        let hint = if f.internal.is_empty() { " (pass --internal-host HOST:PORT)" } else { "" };
        s.log(&format!("  not verified: {}{hint}", untested.join(", ")));
    }
    let reset = if r.wifi_reset_needed() { "REQUIRED by a remedy above" } else { "NOT REQUIRED" };
    s.log(&format!("Wi-Fi reset: {reset}"));
}

// Print a dry-run plan.
pub fn print_plan(s: &Session, plan: &[(Step, Option<&'static str>)]) {
    s.log(&c("1", "Plan (dry run; a real run re-checks after each step and stops when healthy)"));
    if plan.is_empty() {
        s.log("  nothing to run");
    }
    for (step, blocked) in plan {
        let note = blocked.map(|f| format!("  [skipped: needs {f}]")).unwrap_or_default();
        s.log(&format!("  - {}{note}", step.name()));
    }
}

// Print the repair steps that ran and their outcomes.
pub fn print_actions(s: &Session, actions: &[Action]) {
    s.log(&c("1", "Actions"));
    if actions.is_empty() {
        s.log("  none");
    }
    for a in actions {
        match &a.result {
            Ok(msg) => s.log(&format!("  {}  {}: {msg}", c("32", "ok    "), a.step.name())),
            Err(e) => s.log(&format!("  {}  {}: {e}", c("31", "FAILED"), a.step.name())),
        }
    }
}

// Summarize the snapshot as JSON.
pub fn facts_json(f: &Facts) -> Value {
    let vpns: Vec<Value> =
        f.vpns.iter().map(|v| json!({"name": v.name, "state": v.state, "interface": v.iface})).collect();
    let split: Vec<Value> = f
        .resolvers
        .iter()
        .filter(|r| r.supplemental())
        .map(|r| json!({"domain": r.domain, "nameservers": r.nameservers, "interface": r.iface, "reachable": r.reachable}))
        .collect();
    json!({
        "macos": f.macos,
        "interface": f.phys.as_ref().map(|p| &p.device),
        "service": f.phys.as_ref().map(|p| &p.name),
        "ipv4": f.link.ipv4, "ipv6_global": f.link.ipv6_global,
        "gateway": f.gateway, "default_route_if": f.default_if, "public_route_if": f.public_if,
        "tunnels": f.tunnels(), "vpns": vpns, "split_dns": split,
        "wake_age_secs": f.wake_age, "over_ssh": f.over_ssh,
    })
}

// Render the whole run as JSON.
pub fn to_json(
    r: &Report,
    f: &Facts,
    plan: &[(Step, Option<&'static str>)],
    actions: &[Action],
    s: &Session,
) -> Value {
    let checks: Vec<Value> = r
        .checks
        .iter()
        .map(|c| json!({"name": c.name, "state": c.state.label(), "detail": c.detail}))
        .collect();
    let findings: Vec<Value> = r
        .findings
        .iter()
        .map(|x| {
            let remedy = match &x.remedy {
                Remedy::Steps(st) => {
                    json!({"kind": "fix", "steps": st.iter().map(Step::name).collect::<Vec<_>>()})
                }
                Remedy::Manual(t) => json!({"kind": "manual", "text": t}),
                Remedy::Reboot(t) => json!({"kind": "reboot", "text": t}),
            };
            json!({"code": x.code, "summary": x.summary, "remedy": remedy})
        })
        .collect();
    let plan: Vec<Value> = plan.iter().map(|(st, b)| json!({"step": st.name(), "blocked_by": b})).collect();
    let actions: Vec<Value> = actions
        .iter()
        .map(|a| match &a.result {
            Ok(m) => json!({"step": a.step.name(), "ok": true, "message": m}),
            Err(e) => json!({"step": a.step.name(), "ok": false, "message": e}),
        })
        .collect();
    json!({
        "healthy": r.healthy(), "checks": checks, "findings": findings,
        "wifi_reset_required": r.wifi_reset_needed(), "plan": plan, "actions": actions,
        "facts": facts_json(f), "log": s.log_path, "state_dir": s.dir,
    })
}

// Tests for the text and JSON renderings.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::assess::tests::{healthy, internal, route, with_vpn};
    use crate::assess::{assess, Finding};
    use crate::parse::Resolver;
    use crate::run::tests::{logged, session};

    // Build a finding with a remedy.
    fn finding(remedy: Remedy) -> Finding {
        Finding { code: "x", summary: "something".into(), remedy }
    }

    // Check every remedy wording in diagnose and fix mode.
    #[test]
    fn test_remedy_text_variants() {
        let mut f = healthy();
        let vpn = Remedy::Steps(vec![
            Step::ReconnectVpn("W".into()),
            Step::FlushRoutes,
            Step::ReconnectVpn("X".into()),
        ]);
        assert_eq!(
            remedy_text(&vpn, &f, None),
            "fixable: reconnect VPN \"W\", then route -n flush (broad), then reconnect VPN \"X\"  ->  \
             fix-network fix --vpn --allow-route-flush"
        );
        f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "en0", None)];
        assert!(remedy_text(&vpn, &f, None).ends_with("(repeat your --internal-host)"));
        let all = Gates { vpn: true, allow_route_flush: true, ..Gates::default() };
        assert_eq!(
            remedy_text(&vpn, &f, Some(&all)),
            "fixable but not run: reconnect VPN \"W\", then route -n flush (broad), then reconnect VPN \"X\""
        );
        assert!(remedy_text(&vpn, &f, Some(&Gates::default())).ends_with("(add --vpn --allow-route-flush)"));
        assert_eq!(remedy_text(&Remedy::Manual("call".into()), &f, None), "not fixable by this tool: call");
        assert_eq!(remedy_text(&Remedy::Reboot("now".into()), &f, None), "reboot needed: now");
    }

    // Check the report text: checks, findings, untested hint, Wi-Fi reset and details.
    #[test]
    fn test_print_report_and_details() {
        let s = session(true);
        let mut f = with_vpn(healthy());
        f.wake_age = None;
        f.routes.push(route("169.254", "UCS", "utun4"));
        f.routes.push(route("10.9/16", "UCS", "utun9"));
        f.resolvers.push(Resolver {
            domain: Some("lab.example".into()),
            nameservers: vec!["10.0.0.9".into()],
            iface: Some("utun4".into()),
            reachable: Some(false),
            ..Resolver::default()
        });
        f.link.ipv4 = None;
        f.link.ipv6_global = false;
        let r = assess(&f, &[]);
        print_report(&s, &r, &f, None, true);
        let text = logged(&s);
        assert!(text.contains("Local network      FAILED"));
        assert!(text.contains("Wi-Fi reset: REQUIRED by a remedy above"));
        assert!(text.contains("tunnel utun4: 1 routes") && text.contains("(not reachable)"));
        assert!(text.contains("stale route 10.9/16 -> x via utun9"));
        assert!(text.contains("not verified: Corporate service (pass --internal-host HOST:PORT)"));
        assert!(!text.contains("last wake"));
        let s = session(true);
        let mut f = healthy();
        f.internal = vec![internal(Ok(vec!["10.1.2.3"]), "en0", None)];
        f.internal[0].target.port = None;
        f.wake_age = Some(600);
        print_report(&s, &assess(&f, &[]), &f, None, true);
        let text = logged(&s);
        assert!(
            text.contains("  not verified: Corporate service\n")
                && text.contains("last wake from sleep 10 min ago")
        );
        assert!(text.contains("no problems found") && text.contains("interface en0 (Wi-Fi, Wi-Fi)"));
        let s = session(true);
        print_details(&s, &crate::facts::Facts::default());
        assert!(logged(&s).contains("interface - |"));
    }

    // Check the plan and actions listings, empty and filled.
    #[test]
    fn test_print_plan_and_actions() {
        let s = session(true);
        print_plan(&s, &[]);
        print_plan(&s, &[(Step::FlushDns, None), (Step::FixProxy, Some("--fix-proxy"))]);
        print_actions(&s, &[]);
        let actions = [
            Action { step: Step::FlushDns, result: Ok("DNS cache flushed".into()) },
            Action { step: Step::RenewDhcp, result: Err("sudo: a password is required".into()) },
        ];
        print_actions(&s, &actions);
        let text = logged(&s);
        assert!(
            text.contains("nothing to run")
                && text.contains("- disable unreachable proxies  [skipped: needs --fix-proxy]")
        );
        assert!(text.contains("  none") && text.contains("ok      flush DNS cache: DNS cache flushed"));
        assert!(text.contains("FAILED  renew DHCP lease: sudo: a password is required"));
    }

    // Check the JSON rendering of checks, every remedy kind, plan, actions and facts.
    #[test]
    fn test_to_json_shapes() {
        let s = session(true);
        let f = with_vpn(healthy());
        let mut r = assess(&f, &[]);
        r.findings = vec![
            finding(Remedy::Steps(vec![Step::FlushDns])),
            finding(Remedy::Manual("m".into())),
            finding(Remedy::Reboot("r".into())),
        ];
        let plan = [(Step::FixProxy, Some("--fix-proxy"))];
        let actions = [
            Action { step: Step::FlushDns, result: Ok("done".into()) },
            Action { step: Step::RenewDhcp, result: Err("no".into()) },
        ];
        let j = to_json(&r, &f, &plan, &actions, &s);
        assert_eq!(j["healthy"], true);
        assert_eq!(j["findings"][0]["remedy"]["steps"][0], "flush DNS cache");
        assert_eq!(
            (j["findings"][1]["remedy"]["kind"].as_str(), j["findings"][2]["remedy"]["kind"].as_str()),
            (Some("manual"), Some("reboot"))
        );
        assert_eq!(j["plan"][0]["blocked_by"], "--fix-proxy");
        assert_eq!(
            (j["actions"][0]["ok"].as_bool(), j["actions"][1]["ok"].as_bool()),
            (Some(true), Some(false))
        );
        assert_eq!(j["facts"]["vpns"][0]["interface"], "utun4");
        assert_eq!(j["facts"]["split_dns"][0]["domain"], "corp.example");
        assert_eq!(colored(State::NotTested).trim_end(), "NOT TESTED");
    }
}
