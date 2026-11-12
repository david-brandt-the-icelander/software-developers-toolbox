// launchd integration on macOS: socket activation and installing the LaunchAgent.
use std::io;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const LABEL: &str = "com.agent-file-operations.afod";
const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(10);

// Escapes text for an XML element.
fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// Builds the LaunchAgent property list that makes launchd own the socket and start afod on demand.
pub fn plist(exe: &Path, sock: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{}</string><string>serve</string></array>
  <key>Sockets</key>
  <dict><key>Listeners</key>
    <dict>
      <key>SockPathName</key><string>{}</string>
      <key>SockPathMode</key><integer>384</integer>
    </dict>
  </dict>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
        xml(&exe.to_string_lossy()),
        xml(&sock.to_string_lossy())
    )
}

// Returns where the LaunchAgent plist lives for a home directory.
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

// Runs a program with arguments, failing if it exits nonzero or outlives the timeout.
pub fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Result<(), String> {
    let name = format!("{program} {}", args.join(" "));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let start = Instant::now();
    loop {
        let status = child.try_wait().map_err(|e| format!("{program}: {e}"))?;
        match status {
            Some(st) if st.success() => return Ok(()),
            Some(st) => return Err(format!("{name} failed: {st}")),
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name} timed out"));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

// Runs a launchctl command with a timeout.
pub fn launchctl(args: &[&str]) -> Result<(), String> {
    run_with_timeout("launchctl", args, LAUNCHCTL_TIMEOUT)
}

// Writes the plist and loads it into the user's launchd domain.
pub fn install(
    exe: &Path,
    sock: &Path,
    home: &Path,
    uid: u32,
    run: &dyn Fn(&[&str]) -> Result<(), String>,
) -> Result<String, String> {
    let path = plist_path(home);
    std::fs::create_dir_all(path.parent().expect("plist has a parent"))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::write(&path, plist(exe, sock)).map_err(|e| format!("{}: {e}", path.display()))?;
    let domain = format!("gui/{uid}");
    let _ = run(&["bootout", &format!("{domain}/{LABEL}")]);
    run(&["bootstrap", &domain, &path.to_string_lossy()])?;
    Ok(format!("installed {}", path.display()))
}

// Unloads the agent and removes its plist.
pub fn uninstall(
    home: &Path,
    uid: u32,
    run: &dyn Fn(&[&str]) -> Result<(), String>,
) -> Result<String, String> {
    let path = plist_path(home);
    let _ = run(&["bootout", &format!("gui/{uid}/{LABEL}")]);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(format!("removed {}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok("not installed".into()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

// Returns the listener launchd opened for us, or None when not started by launchd.
#[cfg(target_os = "macos")]
pub fn activated_listener() -> io::Result<Option<UnixListener>> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    extern "C" {
        fn launch_activate_socket(
            name: *const libc::c_char,
            fds: *mut *mut libc::c_int,
            cnt: *mut usize,
        ) -> libc::c_int;
    }
    let name = CString::new("Listeners").expect("no interior NUL");
    let mut fds: *mut libc::c_int = std::ptr::null_mut();
    let mut cnt = 0usize;
    // SAFETY: launch_activate_socket fills fds with a malloc'd array of cnt descriptors on success.
    let rc = unsafe { launch_activate_socket(name.as_ptr(), &mut fds, &mut cnt) };
    if rc != 0 || cnt == 0 {
        return Ok(None);
    }
    // SAFETY: on success fds points at cnt >= 1 valid descriptors that we now own; the array is freed once.
    unsafe {
        let fd = *fds;
        libc::free(fds.cast());
        Ok(Some(UnixListener::from_raw_fd(fd)))
    }
}

// Returns the listener launchd opened for us; always None off macOS.
#[cfg(not(target_os = "macos"))]
pub fn activated_listener() -> io::Result<Option<UnixListener>> {
    Ok(None)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::cell::RefCell;

    // Checks the plist names the label, program and socket and escapes XML.
    #[test]
    fn test_plist_contents() {
        let p = plist(Path::new("/opt/a&b/afod"), Path::new("/Users/x/.agent-file-operations/sock"));
        assert!(p.contains("<string>com.agent-file-operations.afod</string>"));
        assert!(p.contains("<string>/opt/a&amp;b/afod</string><string>serve</string>"));
        assert!(p.contains("<key>SockPathName</key><string>/Users/x/.agent-file-operations/sock</string>"));
        assert!(p.contains("<integer>384</integer>"));
        assert_eq!(xml("<>&"), "&lt;&gt;&amp;");
    }

    // Checks the plist location.
    #[test]
    fn test_plist_path() {
        assert_eq!(
            plist_path(Path::new("/h")),
            PathBuf::from("/h/Library/LaunchAgents/com.agent-file-operations.afod.plist")
        );
    }

    // Checks install writes the plist and issues bootout then bootstrap.
    #[test]
    fn test_install_runs_launchctl() {
        let home = TempDir::new();
        let calls = RefCell::new(Vec::new());
        let run = |a: &[&str]| {
            calls.borrow_mut().push(a.join(" "));
            Ok(())
        };
        let msg = install(Path::new("/bin/afod"), Path::new("/s"), home.path(), 501, &run).expect("install");
        assert!(msg.starts_with("installed "));
        assert!(plist_path(home.path()).exists());
        let calls = calls.borrow();
        assert_eq!(calls[0], "bootout gui/501/com.agent-file-operations.afod");
        assert!(calls[1].starts_with("bootstrap gui/501 "));
    }

    // Checks bootstrap failure is reported and write failure too.
    #[test]
    fn test_install_failures() {
        let home = TempDir::new();
        let fail = |a: &[&str]| if a[0] == "bootstrap" { Err("nope".to_string()) } else { Ok(()) };
        assert_eq!(install(Path::new("/b"), Path::new("/s"), home.path(), 1, &fail).unwrap_err(), "nope");
        let file = home.write("f", b"x");
        let err = install(Path::new("/b"), Path::new("/s"), &file, 1, &|_| Ok(())).unwrap_err();
        assert!(err.contains("LaunchAgents") || err.contains("plist"));
    }

    // Checks the write error when the plist path is a directory.
    #[test]
    fn test_install_write_failure() {
        let home = TempDir::new();
        std::fs::create_dir_all(plist_path(home.path())).expect("dir at plist path");
        let err = install(Path::new("/b"), Path::new("/s"), home.path(), 1, &|_| Ok(())).unwrap_err();
        assert!(err.contains(".plist"));
    }

    // Checks uninstall for installed, missing and unremovable plists.
    #[test]
    fn test_uninstall_cases() {
        let home = TempDir::new();
        assert_eq!(uninstall(home.path(), 1, &|_| Ok(())).expect("uninstall"), "not installed");
        install(Path::new("/b"), Path::new("/s"), home.path(), 1, &|_| Ok(())).expect("install");
        assert!(uninstall(home.path(), 1, &|_| Err("x".into())).expect("uninstall").starts_with("removed "));
        std::fs::create_dir_all(plist_path(home.path()).join("sub")).expect("dir");
        assert!(uninstall(home.path(), 1, &|_| Ok(())).is_err());
    }

    // Checks launchctl failure modes using a missing subcommand outcome.
    #[test]
    fn test_launchctl_missing_or_failing() {
        let r = launchctl(&["definitely-not-a-subcommand"]);
        assert!(r.is_err());
    }

    // Checks that outside launchd there is no activated listener.
    #[test]
    fn test_no_activated_listener_outside_launchd() {
        assert!(activated_listener().expect("activation").is_none());
    }

    // Checks run_with_timeout for success, failure, timeout and a missing program.
    #[test]
    fn test_run_with_timeout_outcomes() {
        let t = Duration::from_secs(5);
        assert!(run_with_timeout("true", &[], t).is_ok());
        assert!(run_with_timeout("false", &[], t).unwrap_err().contains("failed"));
        assert!(run_with_timeout("sleep", &["5"], Duration::from_millis(60))
            .unwrap_err()
            .contains("timed out"));
        assert!(run_with_timeout("/no/such/program", &[], t).unwrap_err().starts_with("/no/such/program: "));
    }
}
