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
