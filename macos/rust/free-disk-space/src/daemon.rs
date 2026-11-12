// Background mode: check free space on an awake-time cadence, clean below a threshold, install as a launchd item.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::clean;
use crate::util::{self, die, human_i};

// launchd label of the startup item.
pub const LABEL: &str = "local.free-disk-space";

// Longest sleep between stop-flag checks.
const POLL: Duration = Duration::from_secs(5);

// Free-space level below which cleaning starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Threshold {
    Bytes(i64),
    Percent(f64),
}

impl Threshold {
    // Convert to bytes for a volume of the given size.
    fn limit(self, total: i64) -> i64 {
        match self {
            Threshold::Bytes(b) => b,
            Threshold::Percent(p) => (total as f64 * p / 100.0) as i64,
        }
    }
}

// Validated daemon settings plus the original argument text for the launchd plist.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub threshold: Threshold,
    pub cadence: Duration,
    pub targets: Vec<String>,
    pub dev_root: String,
    threshold_text: String,
    cadence_text: String,
}

// Parse a threshold such as 30G, 500M or 10%.
fn parse_threshold(text: &str) -> Result<Threshold, String> {
    let text = text.trim();
    let Some(pct) = text.strip_suffix('%') else {
        let bytes = util::parse_size(text)?;
        return if bytes > 0 {
            Ok(Threshold::Bytes(bytes))
        } else {
            Err(format!("threshold must be positive: {text}"))
        };
    };
    match pct.trim().parse::<f64>() {
        Ok(p) if p > 0.0 && p < 100.0 => Ok(Threshold::Percent(p)),
        _ => Err(format!("percentage must be between 0 and 100: {text}")),
    }
}

// Parse a cadence such as 90s, 15m or 2h (a bare number is minutes).
fn parse_cadence(text: &str) -> Result<Duration, String> {
    let lower = text.trim().to_lowercase();
    let (number, unit) = match lower.chars().last() {
        Some(u @ ('s' | 'm' | 'h')) => (&lower[..lower.len() - 1], u),
        _ => (lower.as_str(), 'm'),
    };
    let scale = match unit {
        's' => 1.0,
        'h' => 3600.0,
        _ => 60.0,
    };
    match number.trim().parse::<f64>() {
        Ok(n) if n > 0.0 => {
            Duration::try_from_secs_f64(n * scale).map_err(|_| format!("invalid cadence: {text}"))
        }
        _ => Err(format!("invalid cadence: {text}")),
    }
}

impl Settings {
    // Validate command-line text into settings.
    pub fn parse(
        threshold: &str,
        cadence: &str,
        targets: Vec<String>,
        dev_root: &str,
    ) -> Result<Self, String> {
        Ok(Self {
            threshold: parse_threshold(threshold)?,
            cadence: parse_cadence(cadence)?,
            targets,
            dev_root: dev_root.to_string(),
            threshold_text: threshold.trim().to_string(),
            cadence_text: cadence.trim().to_string(),
        })
    }
}

// Print a timestamped log line.
fn log(msg: &str) {
    println!("[{}] {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
}

// Check once and clean if free space is below the threshold; return true if a cleanup ran.
fn check(s: &Settings, space: Option<(i64, i64)>, clean_now: &mut dyn FnMut()) -> bool {
    let Some((total, free)) = space else {
        log("skipped: display asleep or volume unreadable");
        return false;
    };
    let limit = s.threshold.limit(total);
    if free >= limit {
        log(&format!("ok: {} free, threshold {}", human_i(free), human_i(limit)));
        return false;
    }
    log(&format!(
        "below threshold: {} free < {}; cleaning {}",
        human_i(free),
        human_i(limit),
        s.targets.join(", ")
    ));
    clean_now();
    true
}

// Loop on awake time until stopped: probe the disk, then clean when below the threshold.
fn run_loop(
    s: &Settings,
    poll: Duration,
    probe: &dyn Fn() -> Option<(i64, i64)>,
    clean_now: &mut dyn FnMut(),
) {
    let mut next = 0.0;
    while !util::stopped() {
        if util::awake_secs() >= next {
            check(s, probe(), clean_now);
            next = util::awake_secs() + s.cadence.as_secs_f64();
        }
        let wait = Duration::from_secs_f64((next - util::awake_secs()).max(0.0));
        std::thread::sleep(poll.min(wait));
    }
}

// Run the daemon in the foreground until interrupted.
pub fn cmd_run(s: &Settings) {
    util::install_sigint();
    log(&format!(
        "daemon started: every {} awake, threshold {}, targets {}",
        s.cadence_text,
        s.threshold_text,
        s.targets.join(", ")
    ));
    let probe = || util::display_awake().then(util::data_volume_space).flatten();
    let mut clean_now = || clean::cmd_clean(&s.targets, true, &s.dev_root, 30);
    run_loop(s, POLL, &probe, &mut clean_now);
    log("daemon stopped");
}

// Escape text for an XML plist.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// Build the launchd plist that starts the daemon at login and keeps it running.
fn plist(exe: &Path, s: &Settings, log_path: &Path) -> String {
    let mut args = vec![
        exe.to_string_lossy().into_owned(),
        "daemon".into(),
        "run".into(),
        "--threshold".into(),
        s.threshold_text.clone(),
        "--cadence".into(),
        s.cadence_text.clone(),
        "--dev-root".into(),
        s.dev_root.clone(),
        "--targets".into(),
    ];
    args.extend(s.targets.iter().cloned());
    let items: String = args.iter().map(|a| format!("    <string>{}</string>\n", xml(a))).collect();
    let log = xml(&log_path.to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n{items}  </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n  <key>KeepAlive</key>\n  <true/>\n  <key>ProcessType</key>\n  <string>Background</string>\n\
         \x20 <key>LowPriorityIO</key>\n  <true/>\n  <key>Nice</key>\n  <integer>10</integer>\n\
         \x20 <key>EnvironmentVariables</key>\n  <dict>\n    <key>PATH</key>\n    <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>\n  </dict>\n\
         \x20 <key>StandardOutPath</key>\n  <string>{log}</string>\n  <key>StandardErrorPath</key>\n  <string>{log}</string>\n</dict>\n</plist>\n"
    )
}

// Return the user's launch agent plist path.
fn plist_path() -> PathBuf {
    PathBuf::from(util::home()).join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

// Return the daemon log path.
fn log_path() -> PathBuf {
    PathBuf::from(util::home()).join("Library/Logs/free-disk-space.log")
}

// Return the launchd domain target for this user's service.
fn service_target() -> String {
    // SAFETY: getuid has no preconditions.
    format!("gui/{}/{LABEL}", unsafe { libc::getuid() })
}

// Run launchctl with arguments and return its exit code and combined output.
fn launchctl(args: &[&str]) -> (i32, String) {
    let mut cmd = vec!["/bin/launchctl"];
    cmd.extend_from_slice(args);
    util::run(&cmd, 30.0, &[])
        .map_or((-1, "cannot run launchctl".to_string()), |o| (o.code, o.stdout + &o.stderr))
}

// Install and start the daemon as a login item.
pub fn cmd_install(s: &Settings, exe: &Path) {
    let (path, log) = (plist_path(), log_path());
    for dir in [path.parent(), log.parent()].into_iter().flatten() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            die(&format!("cannot create {}: {e}", dir.display()));
        }
    }
    if let Err(e) = std::fs::write(&path, plist(exe, s, &log)) {
        die(&format!("cannot write {}: {e}", path.display()));
    }
    let domain = service_target();
    let _ = launchctl(&["bootout", &domain]);
    let uid_domain = domain.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    let (code, out) = launchctl(&["bootstrap", &uid_domain, &path.to_string_lossy()]);
    if code != 0 {
        let _ = std::fs::remove_file(&path);
        die(&format!("launchctl bootstrap failed: {}", out.trim()));
    }
    println!("Installed {} (starts at login, running now)", path.display());
    println!(
        "Every {} awake: clean {} when free space < {}",
        s.cadence_text,
        s.targets.join(", "),
        s.threshold_text
    );
    println!("Log: {}   Remove with: free-disk-space daemon uninstall", log.display());
}

// Stop the daemon and remove its login item.
pub fn cmd_uninstall() {
    let _ = launchctl(&["bootout", &service_target()]);
    let path = plist_path();
    if path.exists() {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("Removed {}", path.display()),
            Err(e) => die(&format!("cannot remove {}: {e}", path.display())),
        }
    } else {
        println!("Not installed.");
    }
}

// Show whether the login item exists and is running.
pub fn cmd_status() {
    let path = plist_path();
    println!(
        "login item : {} ({})",
        if path.exists() { "installed" } else { "not installed" },
        path.display()
    );
    let running = launchctl(&["print", &service_target()]).0 == 0;
    println!("running    : {}", if running { "yes" } else { "no" });
    println!("log        : {}", log_path().display());
}

// Unit tests for the daemon.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sandbox;

    // Build settings with the defaults and tiny cadence text.
    fn settings(threshold: &str, cadence: &str) -> Settings {
        Settings::parse(threshold, cadence, vec!["caches".into()], "~/dev").unwrap()
    }

    // Check thresholds parse in bytes and percent, and reject bad values.
    #[test]
    fn test_parse_threshold_forms() {
        assert_eq!(parse_threshold("30G"), Ok(Threshold::Bytes(30 << 30)));
        assert_eq!(parse_threshold(" 30Gb "), Ok(Threshold::Bytes(30 << 30)));
        assert_eq!(parse_threshold("12.5%"), Ok(Threshold::Percent(12.5)));
        assert!(parse_threshold("0").is_err());
        assert!(parse_threshold("lots").is_err());
        assert!(parse_threshold("0%").is_err());
        assert!(parse_threshold("100%").is_err());
        assert!(parse_threshold("x%").is_err());
        assert_eq!(Threshold::Percent(10.0).limit(1000), 100);
        assert_eq!(Threshold::Bytes(5).limit(1000), 5);
    }

    // Check cadences parse with each unit and reject bad values.
    #[test]
    fn test_parse_cadence_forms() {
        assert_eq!(parse_cadence("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_cadence("15m"), Ok(Duration::from_secs(900)));
        assert_eq!(parse_cadence("2H"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse_cadence("15"), Ok(Duration::from_secs(900)));
        assert!(parse_cadence("0m").is_err());
        assert!(parse_cadence("soon").is_err());
        assert!(parse_cadence("").is_err());
        assert!(parse_cadence("infh").is_err());
        assert!(parse_cadence("1e300h").is_err());
        assert!(Settings::parse("0", "15m", vec![], "~").is_err());
        assert!(Settings::parse("30G", "no", vec![], "~").is_err());
    }

    // Check a check cleans only when free space is below the threshold.
    #[test]
    fn test_check_decisions() {
        let s = settings("10%", "15m");
        let mut cleaned = 0;
        assert!(!check(&s, None, &mut || cleaned += 1));
        assert!(!check(&s, Some((1000, 100)), &mut || cleaned += 1));
        assert!(check(&s, Some((1000, 99)), &mut || cleaned += 1));
        assert_eq!(cleaned, 1);
    }

    // Check the loop checks immediately, then once per cadence, until stopped.
    #[test]
    fn test_run_loop_cadence_and_stop() {
        let _sb = Sandbox::new();
        let mut s = settings("30G", "15m");
        s.cadence = Duration::from_millis(120);
        let mut runs = 0;
        run_loop(&s, Duration::from_millis(20), &|| Some((100 << 30, 1 << 30)), &mut || {
            runs += 1;
            if runs == 3 {
                util::set_stop(true);
            }
        });
        assert_eq!(runs, 3);
        util::set_stop(true);
        run_loop(&s, POLL, &|| panic!("must not probe"), &mut || {});
    }

    // Check the foreground daemon cleans the sandbox home once and then stops.
    #[test]
    fn test_cmd_run_cleans_and_stops() {
        let sb = Sandbox::new();
        sb.write("home/.cache/big", &[1u8; 8192]);
        let mut s = settings("1%", "15m");
        s.threshold = Threshold::Bytes(i64::MAX);
        let stopper = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(1500));
            util::set_stop(true);
        });
        cmd_run(&s);
        stopper.join().unwrap();
        assert!(!sb.path("home/.cache/big").exists());
    }

    // Check the plist names the program, arguments, log and escapes text.
    #[test]
    fn test_plist_contents() {
        let mut s = settings("10%", "30m");
        s.dev_root = "~/a&b".into();
        let text = plist(Path::new("/opt/bin/free-disk-space"), &s, Path::new("/tmp/l.log"));
        assert!(text.contains("<string>local.free-disk-space</string>"));
        assert!(text.contains("<string>/opt/bin/free-disk-space</string>"));
        for want in
            ["daemon", "run", "--threshold", "10%", "--cadence", "30m", "~/a&amp;b", "--targets", "caches"]
        {
            assert!(text.contains(&format!("<string>{want}</string>")), "{want}");
        }
        assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(text.contains("<key>KeepAlive</key>"));
        assert_eq!(text.matches("/tmp/l.log").count(), 2);
        assert_eq!(xml("<&>"), "&lt;&amp;&gt;");
    }

    // Check install, status and uninstall drive launchctl and manage the plist.
    #[test]
    fn test_install_status_uninstall() {
        let sb = Sandbox::new();
        let calls = sb.path("calls.txt");
        sb.tool("launchctl", &format!("echo \"$@\" >> {}", calls.display()));
        let s = settings("30G", "15m");
        cmd_status();
        cmd_install(&s, Path::new("/opt/bin/free-disk-space"));
        let text = std::fs::read_to_string(plist_path()).unwrap();
        assert!(text.contains("--threshold"));
        assert!(log_path().parent().unwrap().is_dir());
        cmd_status();
        cmd_uninstall();
        assert!(!plist_path().exists());
        cmd_uninstall();
        let log = std::fs::read_to_string(&calls).unwrap();
        assert!(log.contains("bootstrap gui/") && log.contains("bootout gui/") && log.contains("print gui/"));
        assert!(service_target().ends_with("/local.free-disk-space"));
    }

    // Check a failed bootstrap removes the plist and a missing launchctl reads as not running.
    #[test]
    fn test_install_failure_and_missing_launchctl() {
        let sb = Sandbox::new();
        let s = settings("30G", "15m");
        sb.tool("launchctl", "case \"$1\" in bootstrap) echo denied >&2; exit 5;; esac");
        let fail = std::panic::catch_unwind(|| cmd_install(&s, Path::new("/x/fds")));
        assert!(fail.is_err());
        assert!(!plist_path().exists());
        std::fs::remove_file(sb.bin.join("launchctl")).unwrap();
        assert_eq!(launchctl(&["print", "x"]).0, -1);
        cmd_status();
    }

    // Check install reports directory and write failures.
    #[test]
    fn test_install_unwritable_paths() {
        let sb = Sandbox::new();
        let s = settings("30G", "15m");
        sb.write("home/Library", b"a file, not a directory");
        assert!(std::panic::catch_unwind(|| cmd_install(&s, Path::new("/x/fds"))).is_err());
        std::fs::remove_file(sb.path("home/Library")).unwrap();
        std::fs::create_dir_all(plist_path()).unwrap();
        assert!(std::panic::catch_unwind(|| cmd_install(&s, Path::new("/x/fds"))).is_err());
        assert!(std::panic::catch_unwind(cmd_uninstall).is_err());
    }
}
