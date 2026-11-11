// Diagnose and progressively repair macOS Wi-Fi, DNS, routing and VPN.
use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

mod repair;

use clap::{Args, Parser, Subcommand};
use regex::Regex;
use serde_json::{json, Map, Value};

const SENSITIVE: &str = r"(?i)(pass(word)?|secret|token|psk|private.?key|shared.?secret)";

// Wrap text in an ANSI color code when writing to a terminal.
fn c(code: &str, text: &str) -> String {
    if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        return format!("\x1b[{code}m{text}\x1b[0m");
    }
    text.to_string()
}

// Return the root directory for per-run state.
fn state_root() -> PathBuf {
    if let Some(root) = std::env::var_os("FIXNET_STATE") {
        return PathBuf::from(root);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state/fix-network")
}

// Return the first capture group of a pattern in text.
fn capture(pattern: &str, text: &str) -> Option<String> {
    let re = Regex::new(pattern).expect("valid regex");
    re.captures(text).map(|m| m[1].to_string())
}

// Tell whether text is a dotted IPv4 address.
fn is_ipv4(text: &str) -> bool {
    Regex::new(r"^\d+\.\d+\.\d+\.\d+$").expect("valid regex").is_match(text)
}

// Quote a string for a POSIX shell.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

// One run's log directory, saved original settings and list of changes.
struct Session {
    dir: PathBuf,
    log_path: PathBuf,
    original: Map<String, Value>,
    changes: Vec<String>,
    sensitive: Regex,
}

impl Session {
    // Create the timestamped private state directory.
    fn new() -> std::io::Result<Self> {
        let dir = state_root().join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            log_path: dir.join("diagnostic.log"),
            dir,
            original: Map::new(),
            changes: Vec::new(),
            sensitive: Regex::new(SENSITIVE).expect("valid regex"),
        })
    }

    // Append a line, minus sensitive-looking text, to the log and optionally print it.
    fn log_to(&self, msg: &str, show: bool) {
        let line = format!("[{}] {}", chrono::Local::now().format("%H:%M:%S"), msg);
        let kept: Vec<&str> = line.lines().filter(|l| !self.sensitive.is_match(l)).collect();
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            let _ = writeln!(f, "{}", kept.join("\n"));
        }
        if show {
            println!("{msg}");
        }
    }

    // Log and print a line.
    fn log(&self, msg: &str) {
        self.log_to(msg, true);
    }

    // Write the recorded original settings to original.json.
    fn save_original(&self) {
        let text = serde_json::to_string_pretty(&self.original).unwrap_or_default();
        let _ = fs::write(self.dir.join("original.json"), text);
    }

    // Return a recorded original setting as text.
    fn original_text(&self, key: &str) -> String {
        self.original.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
    }
}

// Read a child pipe to a string in a background thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut out = String::new();
        if let Some(mut p) = pipe {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            out = String::from_utf8_lossy(&buf).into_owned();
        }
        out
    })
}

// Run a bounded command, optionally via sudo, and return (exit code, output).
fn sh_opts(cmd: &[&str], timeout: u64, sudo: bool) -> (i32, String) {
    let mut argv: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
    // SAFETY: geteuid has no preconditions.
    if sudo && unsafe { libc::geteuid() } != 0 {
        let mut prefix = vec!["sudo".to_string()];
        if !std::io::stdin().is_terminal() {
            prefix.push("-n".to_string());
        }
        argv.splice(0..0, prefix);
    }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdout(Stdio::piped()).stderr(Stdio::piped());
    if !sudo {
        command.stdin(Stdio::null());
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return (127, e.to_string()),
    };
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = out.join().unwrap_or_default() + &err.join().unwrap_or_default();
                return (status.code().unwrap_or(-1), text.trim().to_string());
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return (124, format!("(timed out after {timeout}s)"));
            }
        }
    }
}

// Run a command with a 15 second limit and return (exit code, output).
fn sh(cmd: &[&str]) -> (i32, String) {
    sh_opts(cmd, 15, false)
}

// Pause for a number of seconds.
fn sleep(secs: u64) {
    thread::sleep(Duration::from_secs(secs));
}

// Network facts found at startup.
#[derive(Clone)]
struct Info {
    macos: String,
    service_order: String,
    wifi_device: Option<String>,
    wifi_service: Option<String>,
    default_route_if: Option<String>,
    vpn_list: String,
    vpns: Vec<(String, bool)>,
}

impl Info {
    // Tell whether the default route uses the Wi-Fi device.
    fn route_is_physical(&self) -> bool {
        self.default_route_if.is_some() && self.default_route_if == self.wifi_device
    }

    // Convert the facts to JSON for original.json.
    fn to_json(&self) -> Value {
        let vpns: Vec<Value> = self.vpns.iter().map(|(n, up)| json!({"name": n, "connected": up})).collect();
        json!({
            "macos": self.macos, "service_order": self.service_order,
            "wifi_device": self.wifi_device, "wifi_service": self.wifi_service,
            "default_route_if": self.default_route_if,
            "default_route_is_physical": self.route_is_physical(),
            "vpn_list": self.vpn_list, "vpns": vpns,
        })
    }
}

// Find the macOS version, Wi-Fi device and service, default-route interface and VPN services.
fn discover() -> Info {
    let order = sh(&["networksetup", "-listnetworkserviceorder"]).1;
    let re = Regex::new(r"\(\d+\)\s+(.+)\n\(Hardware Port: (?:Wi-Fi|AirPort), Device: (\w+)\)")
        .expect("valid regex");
    let (wifi_service, wifi_device) = match re.captures(&order) {
        Some(m) => (Some(m[1].trim().to_string()), Some(m[2].to_string())),
        None => (None, None),
    };
    let route = sh(&["route", "-n", "get", "default"]).1;
    let vpn_list = sh(&["scutil", "--nc", "list"]).1;
    let vpns = vpn_list
        .lines()
        .filter(|l| l.contains("(Connected)") || l.contains("(Disconnected)"))
        .filter_map(|l| capture(r#""([^"]+)""#, l).map(|n| (n, l.contains("(Connected)"))))
        .collect();
    Info {
        macos: sh(&["sw_vers", "-productVersion"]).1,
        service_order: order,
        wifi_device,
        wifi_service,
        default_route_if: capture(r"interface:\s*(\S+)", &route),
        vpn_list,
        vpns,
    }
}

// Save current network settings before any change.
fn record_original(s: &mut Session, info: &Info) {
    let mut put = |key: &str, cmd: &[&str]| {
        s.original.insert(key.to_string(), Value::String(sh(cmd).1));
    };
    if let Some(svc) = info.wifi_service.as_deref() {
        for (key, flag) in [
            ("getinfo", "-getinfo"),
            ("dns", "-getdnsservers"),
            ("search", "-getsearchdomains"),
            ("web_proxy", "-getwebproxy"),
            ("secure_proxy", "-getsecurewebproxy"),
            ("pac", "-getautoproxyurl"),
            ("auto_discovery", "-getproxyautodiscovery"),
        ] {
            put(key, &["networksetup", flag, svc]);
        }
    }
    put("routes", &["netstat", "-rn"]);
    put("scutil_dns", &["scutil", "--dns"]);
    put("scutil_proxy", &["scutil", "--proxy"]);
    if let Some(dev) = info.wifi_device.as_deref() {
        put("ifconfig", &["ifconfig", dev]);
        put("wifi_power", &["networksetup", "-getairportpower", dev]);
        put("wifi_network", &["networksetup", "-getairportnetwork", dev]);
    }
    s.original.insert("discover".to_string(), info.to_json());
    s.save_original();
    let path = s.dir.join("original.json");
    s.log_to(&format!("original settings saved to {}", path.display()), false);
}

// Resolve a name through the system resolver with a time limit.
fn resolve(name: &str, timeout: u64) -> Result<String, String> {
    let (tx, rx) = mpsc::channel();
    let host = name.to_string();
    thread::spawn(move || {
        let result = (host.as_str(), 0)
            .to_socket_addrs()
            .map_err(|e| e.to_string())
            .and_then(|mut a| a.next().map(|x| x.ip().to_string()).ok_or("no address".into()));
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_secs(timeout)).unwrap_or(Err("timeout".to_string()))
}

// Tell whether a TCP connection succeeds within the timeout.
fn tcp(host: &str, port: u16, timeout: u64) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs.into_iter().any(|a| TcpStream::connect_timeout(&a, Duration::from_secs(timeout)).is_ok())
}

// Results of the layered connectivity tests.
struct Health {
    wifi_on: bool,
    ip: String,
    gateway: String,
    gateway_ok: bool,
    captive: bool,
    https_ok: bool,
    dns_ok: bool,
    ip_reachable: bool,
    verdict: &'static str,
}

// Name the lowest failing layer from the test results.
fn verdict(h: &Health) -> &'static str {
    if !h.wifi_on {
        "wifi-off"
    } else if h.ip.is_empty() || h.ip.starts_with("169.254.") {
        "no-dhcp-lease"
    } else if !h.gateway_ok && !h.ip_reachable {
        "gateway-unreachable"
    } else if h.captive {
        "captive-portal"
    } else if !h.ip_reachable && !h.https_ok {
        "upstream-or-routing"
    } else if !h.dns_ok {
        "dns"
    } else if !h.https_ok {
        "https-proxy-or-tls"
    } else {
        "healthy"
    }
}

// Run layered connectivity tests and name the failing layer.
fn health(s: &Session, info: &Info) -> Health {
    let dev = info.wifi_device.as_deref().unwrap_or_default();
    let ip = sh(&["ipconfig", "getifaddr", dev]).1;
    let gw = sh(&["ipconfig", "getoption", dev, "router"]).1;
    let gateway = if is_ipv4(&gw) { gw } else { String::new() };
    let gateway_ok =
        !gateway.is_empty() && sh_opts(&["ping", "-c", "2", "-t", "4", &gateway], 10, false).0 == 0;
    let captive_url = "http://captive.apple.com/hotspot-detect.html";
    let (rc, body) = sh_opts(&["curl", "-sS", "--max-time", "8", captive_url], 12, false);
    let https = ["curl", "-sS", "-o", "/dev/null", "--max-time", "8", "https://www.apple.com/"];
    let mut h = Health {
        wifi_on: sh(&["networksetup", "-getairportpower", dev]).1.contains("On"),
        ip: if is_ipv4(&ip) { ip } else { String::new() },
        gateway,
        gateway_ok,
        captive: rc == 0 && !body.contains("Success"),
        https_ok: sh_opts(&https, 12, false).0 == 0,
        dns_ok: resolve("www.apple.com", 6).is_ok(),
        // An IP-literal connection separates DNS failures from routing failures.
        ip_reachable: tcp("1.1.1.1", 443, 4),
        verdict: "",
    };
    h.verdict = verdict(&h);
    s.log(&format!(
        "  wifi_on={} ip={} gateway={} gw_ping={} captive={} ip_reach={} dns={} https={} => {}",
        h.wifi_on,
        if h.ip.is_empty() { "-" } else { &h.ip },
        if h.gateway.is_empty() { "-" } else { &h.gateway },
        h.gateway_ok,
        h.captive,
        h.ip_reachable,
        h.dns_ok,
        h.https_ok,
        c("1", h.verdict)
    ));
    h
}

// Print VPN and split-DNS state and return scoped DNS domains.
fn vpn_report(s: &Session, info: &Info) -> Vec<String> {
    s.log(&c("1", "VPN"));
    let note = if info.route_is_physical() {
        ""
    } else {
        "  (NOT the Wi-Fi device - a VPN/other interface owns it)"
    };
    let route_if = info.default_route_if.as_deref().unwrap_or("None");
    s.log(&format!("  default route interface: {route_if}{note}"));
    if info.vpns.is_empty() {
        s.log("  no scutil-managed VPN services (third-party clients need their own CLI/UI)");
    }
    for (name, up) in &info.vpns {
        s.log(&format!("  {name}: {}", if *up { "connected" } else { "disconnected" }));
    }
    let dns = sh(&["scutil", "--dns"]).1;
    let re = Regex::new(r"domain\s+:\s+(\S+)").expect("valid regex");
    let mut doms: Vec<String> = re
        .captures_iter(&dns)
        .map(|m| m[1].to_string())
        .filter(|d| !d.ends_with("arpa") && !d.ends_with("local"))
        .collect();
    doms.sort();
    doms.dedup();
    if !doms.is_empty() {
        let shown: Vec<&str> = doms.iter().take(10).map(String::as_str).collect();
        s.log(&format!("  scoped/split DNS domains: {}", shown.join(", ")));
    }
    doms
}

// Check name resolution, route and TCP reachability of an internal host.
fn verify_internal(s: &Session, host_arg: Option<&str>, doms: &[String]) {
    let Some(host_arg) = host_arg else {
        let hint = if doms.is_empty() { "" } else { " (candidate domains above)" };
        s.log(&format!("  internal verification INCOMPLETE: pass --internal-host HOST[:PORT]{hint}"));
        return;
    };
    let (host, port) = host_arg.split_once(':').unwrap_or((host_arg, ""));
    let addr = resolve(host, 6);
    let shown = match &addr {
        Ok(a) => a.clone(),
        Err(e) => format!("FAILED ({e})"),
    };
    s.log(&format!("  internal name {host} -> {shown}"));
    let Ok(addr) = addr else {
        return;
    };
    if !port.is_empty() {
        let open = port.parse().is_ok_and(|p| tcp(host, p, 6));
        s.log(&format!("  tcp {host}:{port} {}", if open { "open" } else { "FAILED" }));
    }
    let route = sh(&["route", "-n", "get", &addr]).1;
    let iface = capture(r"interface:\s*(\S+)", &route).unwrap_or("?".to_string());
    s.log(&format!("  route to {addr} uses interface {iface}"));
}

// Create the session, discover the network and record original settings.
fn prepare(label: &str) -> Result<(Session, Info), ExitCode> {
    if !cfg!(target_os = "macos") {
        eprintln!("error: fix-network works on macOS only");
        return Err(ExitCode::from(2));
    }
    let mut s = Session::new().map_err(|e| {
        eprintln!("error: cannot create state directory: {e}");
        ExitCode::FAILURE
    })?;
    s.log(&format!("{}  (log: {})", c("1", &format!("fix-network {label}")), s.log_path.display()));
    let info = discover();
    let none = "None".to_string();
    s.log(&format!(
        "macOS {} | Wi-Fi device {} | service {}",
        info.macos,
        info.wifi_device.as_ref().unwrap_or(&none),
        info.wifi_service.as_ref().unwrap_or(&none)
    ));
    if info.wifi_device.is_none() {
        eprintln!("error: no Wi-Fi hardware port found");
        return Err(ExitCode::FAILURE);
    }
    record_original(&mut s, &info);
    let ssid = s.original_text("wifi_network");
    let shown = if ssid.contains("Current") { ssid.as_str() } else { "(not reported / not associated)" };
    s.log(&format!("  Wi-Fi network: {shown}"));
    Ok((s, info))
}

// Print the final summary and log locations.
fn finish(s: &Session, h: &Health) {
    s.log(&c("1", "Summary"));
    s.log(&format!("  layer verdict : {}", h.verdict));
    let changes = if s.changes.is_empty() { "none".to_string() } else { s.changes.join(", ") };
    s.log(&format!("  changes made  : {changes}"));
    s.log(&format!("  log           : {}", s.log_path.display()));
    s.log(&format!("  original state: {}", s.dir.join("original.json").display()));
    if h.verdict == "healthy" && !s.changes.is_empty() {
        s.log("  note: restored by the steps above; the step list is not proof of which one was the cause.");
    }
}

// Run the read-only diagnosis.
fn cmd_diagnose(args: &DiagnoseArgs) -> ExitCode {
    let (s, info) = match prepare("diagnose") {
        Ok(v) => v,
        Err(code) => return code,
    };
    let h = health(&s, &info);
    let doms = vpn_report(&s, &info);
    verify_internal(&s, args.internal_host.as_deref(), &doms);
    finish(&s, &h);
    ExitCode::SUCCESS
}

// Diagnose, repair, wait, and verify again.
fn cmd_fix(args: &FixArgs) -> ExitCode {
    let (mut s, info) = match prepare("fix") {
        Ok(v) => v,
        Err(code) => return code,
    };
    repair::fix(&mut s, &info, args);
    s.log(&c("1", "Settling 15s, re-checking"));
    sleep(15);
    let h = health(&s, &info);
    let doms = vpn_report(&s, &info);
    verify_internal(&s, args.internal_host.as_deref(), &doms);
    if h.verdict == "healthy" {
        s.log("  connectivity re-verified after settling");
    }
    finish(&s, &h);
    if h.verdict == "healthy" {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// Force Wi-Fi power on.
fn cmd_recover() -> ExitCode {
    if !cfg!(target_os = "macos") {
        eprintln!("macOS only");
        return ExitCode::FAILURE;
    }
    let out = sh(&["networksetup", "-listallhardwareports"]).1;
    let re = Regex::new(r"Hardware Port: (?:Wi-Fi|AirPort)\nDevice: (\w+)").expect("valid regex");
    for m in re.captures_iter(&out) {
        let dev = &m[1];
        let msg = sh(&["networksetup", "-setairportpower", dev, "on"]).1;
        println!("{}", if msg.is_empty() { format!("Wi-Fi on ({dev})") } else { msg });
    }
    ExitCode::SUCCESS
}

// Command-line interface.
#[derive(Parser)]
#[command(
    version,
    about = "Diagnose and repair macOS Wi-Fi/DNS/routing/VPN, layer by layer.",
    after_help = "examples:\n  fix-network diagnose --internal-host wiki.corp.example:443\n  \
                  fix-network fix --vpn --internal-host wiki.corp.example:443\n  fix-network recover"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

// Available subcommands.
#[derive(Subcommand)]
enum Cmd {
    /// read-only diagnosis + log
    Diagnose(DiagnoseArgs),
    /// diagnose and repair progressively
    Fix(FixArgs),
    /// force Wi-Fi power on
    Recover,
}

// Options for diagnose.
#[derive(Args)]
struct DiagnoseArgs {
    /// internal service to verify over the VPN
    #[arg(long, value_name = "HOST[:PORT]")]
    internal_host: Option<String>,
}

// Options for fix.
#[derive(Args)]
struct FixArgs {
    /// internal service to verify over the VPN
    #[arg(long, value_name = "HOST[:PORT]")]
    internal_host: Option<String>,
    /// if still failing, disconnect scutil-managed VPNs to test plain internet, then reconnect
    #[arg(long)]
    vpn: bool,
    /// disable system proxies whose server is unreachable
    #[arg(long)]
    fix_proxy: bool,
    /// last resort: route -n flush
    #[arg(long)]
    allow_route_flush: bool,
    /// run repair steps even if internet looks healthy
    #[arg(long)]
    force: bool,
}

// Parse arguments and dispatch to the chosen command.
fn main() -> ExitCode {
    match Cli::parse().command {
        Cmd::Diagnose(a) => cmd_diagnose(&a),
        Cmd::Fix(a) => cmd_fix(&a),
        Cmd::Recover => cmd_recover(),
    }
}

// Unit tests for the pure helpers.
#[cfg(test)]
mod tests {
    use super::*;

    // Build a health result with everything passing.
    fn healthy() -> Health {
        Health {
            wifi_on: true,
            ip: "10.0.0.5".into(),
            gateway: "10.0.0.1".into(),
            gateway_ok: true,
            captive: false,
            https_ok: true,
            dns_ok: true,
            ip_reachable: true,
            verdict: "",
        }
    }

    // Check that each failure maps to the lowest failing layer.
    #[test]
    fn test_verdict_layers() {
        assert_eq!(verdict(&healthy()), "healthy");
        assert_eq!(verdict(&Health { wifi_on: false, ..healthy() }), "wifi-off");
        assert_eq!(verdict(&Health { ip: "169.254.1.2".into(), ..healthy() }), "no-dhcp-lease");
        let no_gw = Health { gateway_ok: false, ip_reachable: false, ..healthy() };
        assert_eq!(verdict(&no_gw), "gateway-unreachable");
        assert_eq!(verdict(&Health { captive: true, ..healthy() }), "captive-portal");
        assert_eq!(verdict(&Health { dns_ok: false, ..healthy() }), "dns");
        assert_eq!(verdict(&Health { https_ok: false, ..healthy() }), "https-proxy-or-tls");
    }

    // Check shell quoting of embedded single quotes.
    #[test]
    fn test_shell_quote_escapes() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }

    // Check that a timed-out command reports exit code 124.
    #[test]
    fn test_sh_timeout() {
        assert_eq!(sh_opts(&["sleep", "5"], 1, false).0, 124);
        assert_eq!(sh(&["no-such-binary-xyz"]).0, 127);
    }
}
