// The OS sandbox around script workers and builds: Landlock and seccomp on Linux, sandbox-exec profiles on macOS.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const SYSTEM_READ: [&str; 12] = [
    "/lib",
    "/lib64",
    "/usr/lib",
    "/usr/lib64",
    "/etc/ld.so.cache",
    "/System/Library",
    "/System/Volumes/Preboot/Cryptexes",
    "/private/var/db/dyld",
    "/usr/share/icu",
    "/dev/urandom",
    "/dev/random",
    "/dev/null",
];
const SYSTEM_WRITE: [&str; 1] = ["/dev/null"];
const LOADERS: [&str; 4] = ["/lib", "/lib64", "/usr/lib", "/usr/lib64"];

// What a sandboxed process may touch: files to read, to write, programs to execute, and whether it has a network.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Policy {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub exec: Vec<PathBuf>,
    pub network: bool,
}

impl Policy {
    // A policy for one program: it may execute itself, read the system libraries, and nothing else yet.
    pub fn for_program(program: &Path) -> Policy {
        let existing =
            |list: &[&str]| list.iter().map(PathBuf::from).filter(|p| p.exists()).collect::<Vec<_>>();
        let mut exec = vec![program.to_path_buf()];
        exec.extend(existing(&LOADERS));
        Policy { read: existing(&SYSTEM_READ), write: existing(&SYSTEM_WRITE), exec, network: false }
    }

    // Adds paths to read, skipping missing ones.
    pub fn reading(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Policy {
        self.read.extend(paths.into_iter().filter(|p| p.exists()));
        self
    }

    // Adds paths to read and write, skipping missing ones.
    pub fn writing(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Policy {
        self.write.extend(paths.into_iter().filter(|p| p.exists()));
        self
    }

    // Adds programs (or directories of programs) that may be executed.
    pub fn executing(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Policy {
        self.exec.extend(paths.into_iter().filter(|p| p.exists()));
        self
    }
}

// Builds a sandbox-exec profile whose paths all arrive as -D parameters, so no path can change the profile's meaning.
pub fn sbpl(p: &Policy) -> (String, Vec<(String, String)>) {
    let mut text =
        String::from("(version 1)\n(deny default)\n(allow signal (target self))\n(allow sysctl-read)\n");
    text.push_str("(allow file-read-metadata)\n(allow process-fork)\n(allow dynamic-code-generation)\n");
    text.push_str("(allow mach-lookup)\n(allow mach-task-name)\n(allow ipc-posix-shm-read-data)\n");
    let mut params = Vec::new();
    let mut rule = |kind: &str, prefix: &str, list: &[PathBuf], params: &mut Vec<(String, String)>| {
        for (i, path) in list.iter().enumerate() {
            let name = format!("{prefix}{i}");
            let op = if path.is_dir() { "subpath" } else { "literal" };
            text.push_str(&format!("(allow {kind} ({op} (param \"{name}\")))\n"));
            params.push((name, path.to_string_lossy().into_owned()));
        }
    };
    rule("file-read*", "R", &p.read, &mut params);
    rule("file-read* file-write*", "W", &p.write, &mut params);
    rule("file-read* process-exec", "X", &p.exec, &mut params);
    if p.network {
        text.push_str("(allow network-outbound)\n(allow system-socket)\n(allow mach-lookup (global-name \"com.apple.dnssd.service\"))\n");
    }
    (text, params)
}

// Builds the command for a program inside the sandbox; on Linux the rules are applied in the child before exec.
pub fn command(program: &Path, args: &[OsString], p: &Policy) -> Result<Command, String> {
    #[cfg(target_os = "macos")]
    {
        let (profile, params) = sbpl(p);
        let mut cmd = Command::new(SANDBOX_EXEC);
        cmd.arg("-p").arg(profile);
        for (k, v) in params {
            cmd.arg("-D").arg(format!("{k}={v}"));
        }
        cmd.arg(program).args(args);
        Ok(cmd)
    }
    #[cfg(target_os = "linux")]
    {
        let prepared = linux::prepare(p)?;
        let mut cmd = Command::new(program);
        cmd.args(args);
        // SAFETY: apply only makes syscalls on descriptors prepared before the fork.
        unsafe { std::os::unix::process::CommandExt::pre_exec(&mut cmd, move || linux::apply(&prepared)) };
        Ok(cmd)
    }
}

// Checks the OS can sandbox scripts; unsigned scripts never run without it.
pub fn available() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return if Path::new(SANDBOX_EXEC).is_file() {
        Ok(())
    } else {
        Err(format!("{SANDBOX_EXEC} is missing"))
    };
    #[cfg(target_os = "linux")]
    return match linux::abi() {
        n if n >= 1 => Ok(()),
        _ => Err("Landlock is not enabled in this kernel; unsigned scripts cannot run without it".into()),
    };
}

#[cfg(target_os = "linux")]
pub mod linux {
    // Landlock rules and a seccomp socket filter, prepared before fork and applied in the child.
    use super::Policy;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: u32 = 1;
    const EXECUTE: u64 = 1;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
    const TRUNCATE: u64 = 1 << 14;
    const NET_ALL: u64 = 0b11;
    const FILE_ONLY: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | (1 << 15);
    const AF_UNIX: u32 = 1;
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = 0xc000_00b7;
    const RET_ALLOW: u32 = 0x7fff_0000;
    const RET_EPERM: u32 = 0x0005_0000 | libc::EPERM as u32;
    const RET_KILL: u32 = 0x8000_0000;

    // The ruleset attribute for landlock_create_ruleset (fields after the first are newer ABIs).
    #[repr(C)]
    struct RulesetAttr {
        fs: u64,
        net: u64,
    }

    // The path rule for landlock_add_rule.
    #[repr(C, packed)]
    struct PathBeneath {
        allowed: u64,
        parent_fd: i32,
    }

    // One classic BPF instruction.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SockFilter {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }

    // A BPF program for seccomp.
    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const SockFilter,
    }

    // A ruleset and, when the network is denied, the seccomp program, both built before fork.
    pub struct Prepared {
        ruleset: OwnedFd,
        filter: Option<Vec<SockFilter>>,
    }

    // SAFETY: Prepared holds an owned descriptor and plain data; the filter's pointer is only used in the child.
    unsafe impl Send for Prepared {}
    // SAFETY: nothing in Prepared is mutated after construction.
    unsafe impl Sync for Prepared {}

    // The Landlock ABI version, or a value below 1 when Landlock is unavailable.
    pub fn abi() -> i64 {
        // SAFETY: querying the version takes no pointers.
        unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        }
    }

    // Every filesystem right this ABI can restrict.
    fn handled_fs(abi: i64) -> u64 {
        match abi {
            1 => (1 << 13) - 1,
            2 => (1 << 14) - 1,
            3 | 4 => (1 << 15) - 1,
            _ => (1 << 16) - 1,
        }
    }

    // Adds a rule giving `rights` beneath a path (masked to file rights when the path is a file).
    fn add_rule(ruleset: &OwnedFd, path: &std::path::Path, rights: u64, handled: u64) -> Result<(), String> {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| format!("{}: NUL in path", path.display()))?;
        // SAFETY: c is a valid C string; O_PATH opens no file content.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("{}: {}", path.display(), io::Error::last_os_error()));
        }
        // SAFETY: fd was just opened and is owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mask = if path.is_dir() { handled } else { handled & FILE_ONLY };
        let attr = PathBeneath { allowed: rights & mask, parent_fd: fd.as_raw_fd() };
        // SAFETY: attr is a live, correctly laid out rule for the duration of the call.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                RULE_PATH_BENEATH,
                &attr as *const PathBeneath,
                0u32,
            )
        };
        if rc != 0 {
            return Err(format!("{}: landlock rule: {}", path.display(), io::Error::last_os_error()));
        }
        Ok(())
    }

    // The ruleset attribute and its size for an ABI: every filesystem right, and TCP when the network is denied (ABI 4+).
    fn ruleset_attr(abi: i64, network: bool) -> Result<(RulesetAttr, usize), String> {
        if abi < 1 {
            return Err("Landlock is not enabled in this kernel".into());
        }
        let net = if abi >= 4 && !network { NET_ALL } else { 0 };
        Ok((RulesetAttr { fs: handled_fs(abi), net }, if abi >= 4 { 16 } else { 8 }))
    }

    // Builds the ruleset for a policy; nothing is enforced until `apply` runs in the child.
    pub fn prepare(p: &Policy) -> Result<Prepared, String> {
        let (attr, size) = ruleset_attr(abi(), p.network)?;
        let handled = attr.fs;
        // SAFETY: attr is live and size matches the fields the kernel will read.
        let fd = unsafe {
            libc::syscall(libc::SYS_landlock_create_ruleset, &attr as *const RulesetAttr, size, 0u32)
        };
        if fd < 0 {
            return Err(format!("landlock: {}", io::Error::last_os_error()));
        }
        // SAFETY: the kernel returned a new descriptor that we now own.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        let read = READ_FILE | READ_DIR;
        let write = handled & !EXECUTE;
        for path in &p.read {
            add_rule(&ruleset, path, read, handled)?;
        }
        for path in &p.write {
            add_rule(&ruleset, path, write, handled)?;
        }
        for path in &p.exec {
            add_rule(&ruleset, path, read | EXECUTE, handled)?;
        }
        Ok(Prepared { ruleset, filter: (!p.network).then(|| socket_filter().to_vec()) })
    }

    // Exposes the ruleset attribute as plain numbers for tests.
    #[cfg(test)]
    pub fn linux_attr(abi: i64, network: bool) -> Result<(u64, u64, usize), String> {
        ruleset_attr(abi, network).map(|(a, n)| (a.fs, a.net, n))
    }

    // The seccomp program: refuse sockets other than Unix sockets, io_uring and the x32 ABI; other architectures are killed.
    fn socket_filter() -> [SockFilter; 11] {
        let f = |code: u16, jt: u8, jf: u8, k: u32| SockFilter { code, jt, jf, k };
        let (ld, jeq, jge, ret) = (0x20, 0x15, 0x35, 0x06);
        [
            f(ld, 0, 0, 4),
            f(jeq, 1, 0, ARCH),
            f(ret, 0, 0, RET_KILL),
            f(ld, 0, 0, 0),
            f(jge, 4, 0, 0x4000_0000),
            f(jeq, 0, 2, libc::SYS_socket as u32),
            f(ld, 0, 0, 16),
            f(jeq, 2, 1, AF_UNIX),
            f(jeq, 0, 1, libc::SYS_io_uring_setup as u32),
            f(ret, 0, 0, RET_EPERM),
            f(ret, 0, 0, RET_ALLOW),
        ]
    }

    // Enforces the prepared rules on the calling process; runs between fork and exec, so it only makes syscalls.
    #[cfg_attr(all(coverage_nightly, test), coverage(off))]
    pub fn apply(p: &Prepared) -> io::Result<()> {
        let check = |rc: i64| if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(()) };
        // SAFETY: prctl and the Landlock and seccomp syscalls take only plain values and pointers to live data.
        unsafe {
            check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) as i64)?;
            let proc_self =
                libc::open(c"/proc/self".as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC);
            if proc_self >= 0 {
                let attr = PathBeneath { allowed: READ_FILE | READ_DIR, parent_fd: proc_self };
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    p.ruleset.as_raw_fd(),
                    RULE_PATH_BENEATH,
                    &attr as *const PathBeneath,
                    0u32,
                );
                libc::close(proc_self);
            }
            check(libc::syscall(libc::SYS_landlock_restrict_self, p.ruleset.as_raw_fd(), 0u32))?;
            if let Some(filter) = &p.filter {
                let prog = SockFprog { len: filter.len() as u16, filter: filter.as_ptr() };
                check(libc::prctl(libc::PR_SET_SECCOMP, 2, &prog as *const SockFprog, 0, 0) as i64)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks the profile carries every path as a parameter and the network only when allowed.
    #[test]
    fn test_sbpl() {
        let t = TempDir::new();
        let f = t.write("f", b"");
        let p = Policy {
            read: vec![t.path().to_path_buf()],
            write: vec![f.clone()],
            exec: vec![f.clone()],
            network: false,
        };
        let (text, params) = sbpl(&p);
        assert!(text.starts_with("(version 1)\n(deny default)"));
        assert!(text.contains("(allow file-read* (subpath (param \"R0\")))"));
        assert!(text.contains("(allow file-read* file-write* (literal (param \"W0\")))"));
        assert!(text.contains("(allow file-read* process-exec (literal (param \"X0\")))"));
        assert!(
            !text.contains("network-outbound") && !text.contains(&t.path().to_string_lossy().into_owned())
        );
        assert_eq!(params[0], ("R0".to_string(), t.path().to_string_lossy().into_owned()));
        let (net, _) = sbpl(&Policy { network: true, ..p });
        assert!(net.contains("(allow network-outbound)"));
    }

    // Checks the builder keeps only existing paths.
    #[test]
    fn test_policy_builder() {
        let t = TempDir::new();
        let p = Policy::for_program(Path::new("/bin/sh"))
            .reading([t.path().to_path_buf(), t.path().join("missing")])
            .writing([t.path().join("missing")])
            .executing([PathBuf::from("/nonexistent")]);
        assert!(p.read.contains(&t.path().to_path_buf()) && !p.read.contains(&t.path().join("missing")));
        assert!(!p.write.contains(&t.path().join("missing")) && p.exec[0] == Path::new("/bin/sh"));
        assert!(!p.exec.contains(&PathBuf::from("/nonexistent")));
        assert!(!p.network);
    }

    // Checks the ruleset attribute for each ABI generation.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_ruleset_attr() {
        use linux::*;
        let _ = abi();
        assert!(linux_attr(0, false).is_err());
        assert_eq!(linux_attr(1, false).expect("v1"), ((1 << 13) - 1, 0, 8));
        assert_eq!(linux_attr(2, true).expect("v2"), ((1 << 14) - 1, 0, 8));
        assert_eq!(linux_attr(3, false).expect("v3"), ((1 << 15) - 1, 0, 8));
        assert_eq!(linux_attr(4, false).expect("v4"), ((1 << 15) - 1, 0b11, 16));
        assert_eq!(linux_attr(6, true).expect("v6"), ((1 << 16) - 1, 0, 16));
    }

    // Runs a shell command under a policy and returns (status, stdout).
    #[cfg(target_os = "linux")]
    fn sh(p: &Policy, script: &str) -> (i32, String) {
        let mut cmd = command(Path::new("/bin/sh"), &["-c".into(), script.into()], p).expect("command");
        let out = cmd.output().expect("run");
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }

    // Checks sandbox-exec confines reads to the policy on macOS.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_macos_enforcement() {
        available().expect("sandbox-exec");
        let t = TempDir::new();
        let inside = t.write("in/a.txt", b"inside");
        let outside = t.write("out/b.txt", b"outside");
        let p = Policy::for_program(Path::new("/bin/cat")).reading([t.path().join("in")]);
        let run = |f: &Path| {
            command(Path::new("/bin/cat"), &[f.into()], &p).expect("command").output().expect("run")
        };
        let ok = run(&inside);
        assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
        assert_eq!(ok.stdout, b"inside");
        assert!(!run(&outside).status.success());
    }

    // Checks Landlock confines reads and writes to the policy, and seccomp refuses network sockets.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_enforcement() {
        available().expect("this kernel must support Landlock for the zazz tests");
        let t = TempDir::new();
        let inside = t.write("in/a.txt", b"inside");
        let outside = t.write("out/b.txt", b"outside");
        let base = Policy::for_program(Path::new("/bin/sh"))
            .executing([PathBuf::from("/bin"), PathBuf::from("/usr/bin")]);
        let p = base.clone().reading([t.path().join("in")]).writing([t.path().join("w")]);
        std::fs::create_dir(t.path().join("w")).expect("w");
        let p = Policy { write: [p.write.clone(), vec![t.path().join("w")]].concat(), ..p };
        assert_eq!(sh(&p, &format!("cat {}", inside.display())), (0, "inside".into()));
        assert_ne!(sh(&p, &format!("cat {}", outside.display())).0, 0);
        assert_ne!(sh(&p, &format!("echo x > {}/new", t.path().join("in").display())).0, 0);
        assert_eq!(sh(&p, &format!("echo x > {}/new", t.path().join("w").display())).0, 0);
        assert_ne!(sh(&p, "cat /etc/hostname").0, 0);
        let all = Policy { read: vec![PathBuf::from("/")], ..base.clone() };
        let py = "exec python3 -c 'import socket; socket.socket(socket.AF_INET)'";
        assert_ne!(sh(&all, py).0, 0, "an AF_INET socket must be refused");
        assert_eq!(sh(&all, "exec python3 -c 'import socket; socket.socket(socket.AF_UNIX)'").0, 0);
        let open = Policy { network: true, ..all };
        assert_eq!(sh(&open, py).0, 0);
        assert!(command(
            Path::new("/bin/sh"),
            &[],
            &Policy { read: vec![PathBuf::from("/nonexistent")], ..base }
        )
        .is_err());
    }
}
