// Diagnose and repair macOS network, DNS, routing, proxy and VPN problems.
use std::process::ExitCode;

mod assess;
mod corp;
mod facts;
mod model;
mod parse;
mod repair;
mod report;
mod run;

use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use assess::{assess, Step};
use facts::{gather, Facts};
use parse::{parse_target, Target};
use repair::{exec_step, fix_loop, force_steps, plan, Action, Exec, Gates};
use report::{facts_json, print_actions, print_plan, print_report, to_json};
use run::{c, sh, sh_opts, sleep, Session};

// Save current settings, with secrets redacted, to original.json before any change.
fn record_original(s: &mut Session, f: &Facts) {
    let put = |s: &mut Session, key: &str, cmd: &[&str]| {
        let text = s.redact(&sh(cmd).1);
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
        if p.is_wifi() {
            put(s, "wifi_power", &["networksetup", "-getairportpower", &p.device]);
        }
    }
    put(s, "routes", &["netstat", "-rn"]);
    put(s, "scutil_dns", &["scutil", "--dns"]);
    put(s, "scutil_proxy", &["scutil", "--proxy"]);
    put(s, "vpn_list", &["scutil", "--nc", "list"]);
    s.original.insert("facts".to_string(), facts_json(f));
    s.save_original();
}

// Create the session and take and record the first snapshot.
fn prepare(label: &str, common: &Common) -> Result<(Session, Facts), ExitCode> {
    if !cfg!(target_os = "macos") {
        eprintln!("error: fix-network works on macOS only");
        return Err(ExitCode::from(2));
    }
    let mut s = Session::new(common.json).map_err(|e| {
        eprintln!("error: cannot create state directory: {e}");
        ExitCode::FAILURE
    })?;
    s.log(&format!("{}  (log: {})", c("1", &format!("fix-network {label}")), s.log_path.display()));
    let f = gather(&common.internal_host);
    let dev = f.phys.as_ref().map_or("none".into(), |p| format!("{} ({})", p.device, p.name));
    s.log(&format!("macOS {} | interface {dev}", f.macos));
    record_original(&mut s, &f);
    Ok((s, f))
}

// Print JSON or log locations and return the exit status for the final report.
fn finish(
    s: &Session,
    r: &assess::Report,
    f: &Facts,
    plan: &[(Step, Option<&'static str>)],
    actions: &[Action],
) -> ExitCode {
    if s.quiet {
        println!("{}", serde_json::to_string_pretty(&to_json(r, f, plan, actions, s)).unwrap_or_default());
    }
    s.log(&format!(
        "log: {} | original state: {}",
        s.log_path.display(),
        s.dir.join("original.json").display()
    ));
    if r.healthy() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// Run the read-only diagnosis.
fn cmd_diagnose(a: &Common) -> ExitCode {
    let (s, f) = match prepare("diagnose", a) {
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

// Print the plan a fix would start with, without changing anything.
fn dry_run(s: &Session, a: &FixArgs, f: &Facts, g: &Gates) -> ExitCode {
    let r = assess(f, &[]);
    print_report(s, &r, f, Some(g), a.common.verbose);
    let mut p: Vec<(Step, Option<&'static str>)> = Vec::new();
    if a.force {
        p.extend(force_steps(f).into_iter().map(|st| {
            let blocked = g.blocked(&st);
            (st, blocked)
        }));
    }
    p.extend(plan(&r, g).into_iter().filter(|(st, _)| !p.iter().any(|(q, _)| q == st)).collect::<Vec<_>>());
    print_plan(s, &p);
    finish(s, &r, f, &p, &[])
}

// Diagnose, repair adaptively, settle and verify every requested dimension again.
fn cmd_fix(a: &FixArgs) -> ExitCode {
    let (s, first) = match prepare("fix", &a.common) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let g = gates(a, &first);
    if a.dry_run {
        return dry_run(&s, a, &first, &g);
    }
    let targets: Vec<Target> = a.common.internal_host.clone();
    let mut pending = Some(first);
    let mut regather = || pending.take().unwrap_or_else(|| gather(&targets));
    let run = |cmd: &[&str], timeout: u64, sudo: bool| sh_opts(cmd, timeout, sudo);
    let pause = |secs: u64| sleep(secs);
    let log = |m: &str| s.log(m);
    let x = Exec { run: &run, pause: &pause, log: &log, state_dir: &s.dir };
    s.log(&c("1", "Repair"));
    let (mut facts, actions) = fix_loop(&g, a.force, &mut regather, &mut |st, f| exec_step(&x, st, f), &log);
    if actions.is_empty() {
        s.log("  no step run: healthy, or every remedy needs a flag or manual action (see Diagnosis)");
    } else {
        s.log("Settling 15s, re-checking everything");
        sleep(15);
        facts = gather(&targets);
    }
    let tried: Vec<Step> = actions.iter().map(|x| x.step.clone()).collect();
    let r = assess(&facts, &tried);
    print_report(&s, &r, &facts, Some(&g), a.common.verbose);
    print_actions(&s, &actions);
    let p = plan(&r, &g);
    finish(&s, &r, &facts, &p, &actions)
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

// Parse arguments and dispatch to the chosen command.
fn main() -> ExitCode {
    match Cli::parse().command {
        Cmd::Diagnose(a) => cmd_diagnose(&a),
        Cmd::Fix(a) => cmd_fix(&a),
    }
}

// CLI parsing tests.
#[cfg(test)]
mod tests {
    use super::*;

    // Check that repeated internal hosts and fix flags parse.
    #[test]
    fn test_cli_parses_fix_flags() {
        let cli = Cli::parse_from([
            "fix-network",
            "fix",
            "--vpn",
            "--dry-run",
            "--internal-host",
            "a.corp:443",
            "--internal-host",
            "b.corp",
        ]);
        let Cmd::Fix(a) = cli.command else { panic!("expected fix") };
        assert!(a.vpn && a.dry_run && !a.force);
        assert_eq!(a.common.internal_host.len(), 2);
        assert!(Cli::try_parse_from(["fix-network", "diagnose", "--internal-host", "a:x"]).is_err());
        assert!(Cli::try_parse_from(["fix-network", "recover"]).is_err());
    }
}
