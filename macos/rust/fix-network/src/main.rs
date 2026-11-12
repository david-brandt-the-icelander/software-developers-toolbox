// Diagnose and repair macOS network, DNS, routing, proxy and VPN problems.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
use std::path::Path;
use std::process::ExitCode;

mod assess;
mod corp;
mod facts;
mod model;
mod parse;
mod repair;
mod report;
mod run;
mod sys;

use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use assess::{assess, Report, Step};
use facts::{gather, Facts};
use parse::{parse_target, Target};
use repair::{exec_step, fix_loop, force_steps, plan, Action, Exec, Gates};
use report::{facts_json, print_actions, print_plan, print_report, to_json};
use run::{c, state_root, Session};
use sys::{Real, Sys};

// Exit statuses: healthy, problems remain, and not runnable here.
const OK: u8 = 0;
const PROBLEMS: u8 = 1;
const UNSUPPORTED: u8 = 2;

// Save current settings, with secrets redacted, to original.json before any change.
fn record_original(sys: &dyn Sys, s: &mut Session, f: &Facts) {
    let put = |s: &mut Session, key: &str, cmd: &[&str]| {
        let text = s.redact(&sys.run(cmd, 15, false).1);
        s.original.insert(key.to_string(), Value::String(text));
    };
    if let Some(p) = f.phys.clone() {
        for (key, flag) in [
            ("getinfo", "-getinfo"),
            ("dns", "-getdnsservers"),
            ("search", "-getsearchdomains"),
            ("web_proxy", "-getwebproxy"),
            ("secure_proxy", "-getsecurewebproxy"),
            ("pac", "-getautoproxyurl"),
        ] {
            put(s, key, &["networksetup", flag, &p.name]);
        }
        put(s, "ifconfig", &["ifconfig", &p.device]);
        put(s, "wifi_power", &["networksetup", "-getairportpower", &p.device]);
    }
    put(s, "routes", &["netstat", "-rn"]);
    put(s, "scutil_dns", &["scutil", "--dns"]);
    put(s, "scutil_proxy", &["scutil", "--proxy"]);
    put(s, "vpn_list", &["scutil", "--nc", "list"]);
    s.original.insert("facts".to_string(), facts_json(f));
    s.save_original();
}

// Create the session, then take and record the first snapshot.
fn prepare(sys: &dyn Sys, root: &Path, label: &str, common: &Common) -> Result<(Session, Facts), u8> {
    let mut s = Session::at(root, common.json).map_err(|e| {
        eprintln!("error: cannot create state directory: {e}");
        PROBLEMS
    })?;
    s.log(&format!("{}  (log: {})", c("1", &format!("fix-network {label}")), s.log_path.display()));
    let f = gather(sys, &common.internal_host);
    let dev = f.phys.as_ref().map_or("none".into(), |p| format!("{} ({})", p.device, p.name));
    s.log(&format!("macOS {} | interface {dev}", f.macos));
    record_original(sys, &mut s, &f);
    Ok((s, f))
}

// Print JSON or log locations and return the exit status for the final report.
fn finish(
    s: &Session,
    r: &Report,
    f: &Facts,
    plan: &[(Step, Option<&'static str>)],
    actions: &[Action],
) -> u8 {
    if s.quiet {
        println!("{}", serde_json::to_string_pretty(&to_json(r, f, plan, actions, s)).unwrap_or_default());
    }
    s.log(&format!(
        "log: {} | original state: {}",
        s.log_path.display(),
        s.dir.join("original.json").display()
    ));
    if r.healthy() {
        OK
    } else {
        PROBLEMS
    }
}

// Run the read-only diagnosis.
fn cmd_diagnose(sys: &dyn Sys, root: &Path, a: &Common) -> u8 {
    let (s, f) = match prepare(sys, root, "diagnose", a) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let r = assess(&f, &[]);
    print_report(&s, &r, &f, None, a.verbose);
    let p = plan(&r, &Gates { over_ssh: f.over_ssh, ..Gates::default() });
    finish(&s, &r, &f, &p, &[])
}

// Build the gates from the fix flags.
fn gates(a: &FixArgs, f: &Facts) -> Gates {
    Gates {
        vpn: a.vpn,
        fix_proxy: a.fix_proxy,
        allow_route_flush: a.allow_route_flush,
        allow_remote: a.allow_remote,
        over_ssh: f.over_ssh,
    }
}

// Return the steps a fix would start with: the --force ladder first, then the findings' steps.
fn start_plan(force: bool, r: &Report, f: &Facts, g: &Gates) -> Vec<(Step, Option<&'static str>)> {
    let mut p: Vec<(Step, Option<&'static str>)> = Vec::new();
    if force {
        for st in force_steps(f) {
            let blocked = g.blocked(&st);
            p.push((st, blocked));
        }
    }
    for (st, blocked) in plan(r, g) {
        if !p.iter().any(|(q, _)| *q == st) {
            p.push((st, blocked));
        }
    }
    p
}

// Print the plan a fix would start with, without changing anything.
fn dry_run(s: &Session, a: &FixArgs, f: &Facts, g: &Gates) -> u8 {
    let r = assess(f, &[]);
    print_report(s, &r, f, Some(g), a.common.verbose);
    let p = start_plan(a.force, &r, f, g);
    print_plan(s, &p);
    finish(s, &r, f, &p, &[])
}

// Diagnose, repair adaptively, settle and verify every requested dimension again.
fn cmd_fix(sys: &dyn Sys, root: &Path, a: &FixArgs) -> u8 {
    let (s, first) = match prepare(sys, root, "fix", &a.common) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let g = gates(a, &first);
    if a.dry_run {
        return dry_run(&s, a, &first, &g);
    }
    let targets: Vec<Target> = a.common.internal_host.clone();
    let mut pending = Some(first);
    let mut regather = || pending.take().unwrap_or_else(|| gather(sys, &targets));
    let log = |m: &str| s.log(m);
    let x = Exec { sys, log: &log, dir: &s.dir };
    s.log(&c("1", "Repair"));
    let (mut facts, actions) = fix_loop(&g, a.force, &mut regather, &mut |st, f| exec_step(&x, st, f), &log);
    if actions.is_empty() {
        s.log("  no step run: healthy, or every remedy needs a flag or manual action (see Diagnosis)");
    } else {
        s.log("Settling 15s, re-checking everything");
        sys.pause(15);
        facts = gather(sys, &targets);
    }
    let tried: Vec<Step> = actions.iter().map(|x| x.step.clone()).collect();
    let r = assess(&facts, &tried);
    print_report(&s, &r, &facts, Some(&g), a.common.verbose);
    print_actions(&s, &actions);
    let p = plan(&r, &g);
    finish(&s, &r, &facts, &p, &actions)
}

// Dispatch a parsed command on a machine, refusing anything but macOS.
fn run(cli: Cli, sys: &dyn Sys, os: &str, root: &Path) -> u8 {
    if os != "macos" {
        eprintln!("error: fix-network works on macOS only");
        return UNSUPPORTED;
    }
    match cli.command {
        Cmd::Diagnose(a) => cmd_diagnose(sys, root, &a),
        Cmd::Fix(a) => cmd_fix(sys, root, &a),
    }
}

// Command-line interface.
#[derive(Parser)]
#[command(
    version,
    about = "Diagnose and repair macOS network, DNS, routing, proxy and VPN problems.",
    after_help = "examples:\n  fix-network diagnose --internal-host wiki.corp.example:443\n  \
                  fix-network fix --dry-run --internal-host wiki.corp.example:443\n  \
                  fix-network fix --vpn --internal-host wiki.corp.example:443\n\n\
                  exit status: 0 healthy, 1 problems remain, 2 not macOS"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

// Available subcommands.
#[derive(Subcommand)]
enum Cmd {
    /// read-only diagnosis (no root, no changes)
    Diagnose(Common),
    /// diagnose, repair least-disruptive first, then verify again
    Fix(FixArgs),
}

// Options shared by diagnose and fix.
#[derive(Args)]
struct Common {
    /// internal service to verify (DNS, route, TCP); repeatable
    #[arg(long, value_name = "HOST[:PORT]", value_parser = parse_target)]
    internal_host: Vec<Target>,
    /// print the final report as JSON on stdout
    #[arg(long)]
    json: bool,
    /// also show interfaces, tunnels, split DNS, VPN services and stale routes
    #[arg(short, long)]
    verbose: bool,
}

// Options for fix.
#[derive(Args)]
struct FixArgs {
    #[command(flatten)]
    common: Common,
    /// allow disconnecting/reconnecting scutil-managed VPNs
    #[arg(long)]
    vpn: bool,
    /// allow disabling system proxies whose server is unreachable
    #[arg(long)]
    fix_proxy: bool,
    /// allow `route -n flush` as a last resort for stale routes
    #[arg(long)]
    allow_route_flush: bool,
    /// allow steps that can cut an SSH session (DHCP renew, Wi-Fi cycle, VPN reconnect, route flush)
    #[arg(long)]
    allow_remote: bool,
    /// first run flush DNS, clear ARP, renew DHCP and cycle Wi-Fi even if healthy
    #[arg(long)]
    force: bool,
    /// show the diagnosis and repair plan without changing anything
    #[arg(long)]
    dry_run: bool,
}

// Parse arguments and run on the real machine.
fn main() -> ExitCode {
    ExitCode::from(run(Cli::parse(), &Real, std::env::consts::OS, &state_root()))
}

// End-to-end tests of both commands against a scripted Mac.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::facts::tests::{mac, with_vpn};
    use crate::sys::fake::Fake;

    // Return the state root of one test.
    fn root_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fixnet-main-{}-{name}", std::process::id()))
    }

    // Return a fresh, empty state root for one test.
    fn root(name: &str) -> PathBuf {
        let dir = root_path(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    // Run a command line against a scripted Mac and return the exit status and everything logged.
    fn cli(sys: &dyn Sys, name: &str, args: &[&str]) -> (u8, String) {
        let dir = root(name);
        let argv = [&["fix-network"], args].concat();
        let code = run(Cli::parse_from(argv), sys, "macos", &dir);
        let mut log = String::new();
        for entry in fs::read_dir(&dir).unwrap() {
            log.push_str(
                &fs::read_to_string(entry.unwrap().path().join("diagnostic.log")).unwrap_or_default(),
            );
        }
        (code, log)
    }

    // Script a Mac whose VPN is up but whose intranet service drops connections until the VPN restarts.
    fn broken_vpn() -> Fake {
        let sys = with_vpn(mac());
        sys.name("wiki.corp.example", &["10.1.2.3"])
            .on("route -n get 10.1.2.3", &[(0, "  interface: utun4")]);
        sys
    }

    // Check that repeated internal hosts and fix flags parse, and that recover is gone.
    #[test]
    fn test_cli_parses_fix_flags() {
        let args = [
            "fix-network",
            "fix",
            "--vpn",
            "--dry-run",
            "--internal-host",
            "a.corp:443",
            "--internal-host",
            "b.corp",
        ];
        let Cmd::Fix(a) = Cli::parse_from(args).command else { panic!("expected fix") };
        assert!(a.vpn && a.dry_run && !a.force);
        assert_eq!(a.common.internal_host.len(), 2);
        assert!(Cli::try_parse_from(["fix-network", "diagnose", "--internal-host", "a:x"]).is_err());
        assert!(Cli::try_parse_from(["fix-network", "recover"]).is_err());
    }

    // Check that anything but macOS is refused with status 2 before touching the machine.
    #[test]
    fn test_run_refuses_other_os() {
        let sys = mac();
        assert_eq!(
            run(Cli::parse_from(["fix-network", "diagnose"]), &sys, "linux", &root("os")),
            UNSUPPORTED
        );
        assert!(sys.calls.borrow().is_empty());
    }

    // Check a healthy diagnosis: status 0, verdict logged, original settings saved, nothing changed.
    #[test]
    fn test_diagnose_healthy() {
        let sys = mac();
        let (code, log) = cli(&sys, "diag-ok", &["diagnose", "-v"]);
        assert_eq!(code, OK);
        assert!(log.contains("Local network      HEALTHY") && log.contains("no problems found"));
        assert!(log.contains("Details") && log.contains("Wi-Fi reset: NOT REQUIRED"));
        assert!(sys.calls.borrow().iter().all(|c| !c.starts_with("sudo") && !c.contains("-setairportpower")));
        let dir = fs::read_dir(root_path("diag-ok")).unwrap().next().unwrap().unwrap().path();
        let original = fs::read_to_string(dir.join("original.json")).unwrap();
        assert!(original.contains("\"wifi_power\"") && original.contains("\"facts\""));
    }

    // Check that a diagnosis with a broken VPN service exits 1 and names the fix command.
    #[test]
    fn test_diagnose_broken_vpn_json() {
        let sys = broken_vpn();
        let (code, log) =
            cli(&sys, "diag-vpn", &["diagnose", "--json", "--internal-host", "wiki.corp.example:443"]);
        assert_eq!(code, PROBLEMS);
        assert!(log.contains("fix-network fix --vpn (repeat your --internal-host)"));
    }

    // Check that the state directory failing to appear exits 1.
    #[test]
    fn test_prepare_state_dir_error() {
        let code =
            run(Cli::parse_from(["fix-network", "diagnose"]), &mac(), "macos", Path::new("/proc/no/such"));
        assert_eq!(code, PROBLEMS);
    }

    // Check that fix on a healthy Mac runs nothing and exits 0.
    #[test]
    fn test_fix_healthy_noop() {
        let sys = mac();
        let (code, log) = cli(&sys, "fix-ok", &["fix"]);
        assert_eq!(code, OK);
        assert!(log.contains("no step run"));
        assert!(sys.calls.borrow().iter().all(|c| !c.starts_with("sudo")));
    }

    // Check that fix --vpn reconnects the VPN, verifies the intranet again and exits 0.
    #[test]
    fn test_fix_vpn_reconnect_end_to_end() {
        let sys = broken_vpn();
        let up = "Connected\n  InterfaceName : utun4\n";
        // Status: first gather, before stop, after stop, after start (then repeated by later gathers).
        sys.on("scutil --nc status Work", &[(0, up), (0, up), (0, "Disconnected"), (0, up)])
            .on("scutil --nc stop Work", &[(0, "")])
            .on("scutil --nc start Work", &[(0, "")]);
        let args = ["fix", "--vpn", "--internal-host", "wiki.corp.example:443"];
        let (code, log) = cli(&Reconnects { inner: &sys }, "fix-vpn", &args);
        assert_eq!(code, OK, "{log}");
        assert!(log.contains("reconnect VPN \"Work\": \"Work\" connected"));
        assert!(log.contains("Corporate service  HEALTHY"));
        assert_eq!(sys.count("scutil --nc start Work"), 1);
        assert_eq!(sys.count("networksetup -setairportpower en0 off"), 0);
    }

    // A scripted Mac whose intranet port opens once the VPN has been started again.
    struct Reconnects<'a> {
        inner: &'a Fake,
    }

    impl Sys for Reconnects<'_> {
        // Open the intranet port when the VPN starts.
        fn run(&self, cmd: &[&str], timeout: u64, sudo: bool) -> (i32, String) {
            if cmd == ["scutil", "--nc", "start", "Work"] {
                self.inner.listen("10.1.2.3", 443);
            }
            self.inner.run(cmd, timeout, sudo)
        }
        // Delegate to the scripted Mac.
        fn resolve(&self, name: &str, timeout: u64) -> Result<Vec<std::net::IpAddr>, String> {
            self.inner.resolve(name, timeout)
        }
        // Delegate to the scripted Mac.
        fn tcp(&self, addrs: &[std::net::IpAddr], port: u16, timeout: u64) -> bool {
            self.inner.tcp(addrs, port, timeout)
        }
        // Delegate to the scripted Mac.
        fn pause(&self, secs: u64) {
            self.inner.pause(secs)
        }
        // Delegate to the scripted Mac.
        fn now(&self) -> u64 {
            self.inner.now()
        }
        // Delegate to the scripted Mac.
        fn over_ssh(&self) -> bool {
            self.inner.over_ssh()
        }
        // Delegate to the scripted Mac.
        fn watchdog(&self, dir: &Path, dev: &str, delay: u64) -> std::io::Result<std::process::Child> {
            self.inner.watchdog(dir, dev, delay)
        }
    }

    // Check that a failed reconnect leaves status 1, records FAILED and gives manual advice.
    #[test]
    fn test_fix_vpn_reconnect_fails() {
        let sys = broken_vpn();
        sys.on("scutil --nc stop Work", &[(1, "permission denied")]);
        let (code, log) =
            cli(&sys, "fix-vpn-fail", &["fix", "--vpn", "--internal-host", "wiki.corp.example:443"]);
        assert_eq!(code, PROBLEMS);
        assert!(log.contains("FAILED  reconnect VPN \"Work\""));
        assert!(log.contains("not fixable by this tool: Reconnecting \"Work\" did not help"));
    }

    // Check dry runs, with and without --force, change nothing and list blocked steps.
    #[test]
    fn test_fix_dry_run_plans() {
        let sys = broken_vpn();
        let (code, log) = cli(
            &sys,
            "dry",
            &["fix", "--dry-run", "--json", "--force", "--internal-host", "wiki.corp.example:443"],
        );
        assert_eq!(code, PROBLEMS);
        assert!(log.contains("- flush DNS cache") && log.contains("[skipped: needs --vpn]"));
        assert!(sys.calls.borrow().iter().all(|c| !c.starts_with("sudo") && !c.contains("--nc stop")));
        let (_, log) = cli(&mac(), "dry-ok", &["fix", "--dry-run"]);
        assert!(log.contains("nothing to run"));
    }

    // Check that the start plan puts the force ladder first without repeating steps.
    #[test]
    fn test_start_plan_merges() {
        let f = crate::assess::tests::healthy();
        let mut broken = f.clone();
        broken.public.dns_ok = false;
        let r = assess(&broken, &[]);
        let p = start_plan(true, &r, &f, &Gates::default());
        assert_eq!(p.iter().filter(|(s, _)| *s == Step::FlushDns).count(), 1);
        assert_eq!(p[0].0, Step::FlushDns);
        assert!(start_plan(false, &assess(&f, &[]), &f, &Gates::default()).is_empty());
    }

    // Check a Mac with no network service at all: a diagnosis still runs and records what it can.
    #[test]
    fn test_diagnose_without_any_service() {
        let sys = mac();
        sys.on("networksetup -listnetworkserviceorder", &[(0, "")]);
        let (code, log) = cli(&sys, "no-svc", &["diagnose"]);
        assert_eq!(code, PROBLEMS);
        assert!(log.contains("interface none") && log.contains("No enabled network hardware port"));
        assert_eq!(sys.count("networksetup -getinfo Wi-Fi"), 0);
    }
}
