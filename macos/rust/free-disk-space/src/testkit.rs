// Shared test fixtures: a process-wide lock, a temp sandbox with fake tools, and file helpers.
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::util;

static LOCK: Mutex<()> = Mutex::new(());
static COUNTER: AtomicU32 = AtomicU32::new(0);
const ENV_KEYS: &[&str] = &["HOME", "PATH", "FDS_TOOLS", "FDS_STATE", "FDS_FORCE", "NO_COLOR"];

// A temp directory with fake tools on PATH, a private HOME and serialized access to process state.
pub struct Sandbox {
    pub root: PathBuf,
    pub bin: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
    _guard: MutexGuard<'static, ()>,
}

impl Sandbox {
    // Lock process state, create the directories and point the environment at them.
    pub fn new() -> Self {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("fds-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create bin");
        fs::create_dir_all(root.join("home")).expect("create home");
        let saved = ENV_KEYS.iter().map(|k| (*k, std::env::var_os(k))).collect();
        let path = std::env::join_paths(
            std::iter::once(bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .expect("join PATH");
        std::env::set_var("PATH", path);
        std::env::set_var("HOME", root.join("home"));
        std::env::set_var("FDS_TOOLS", &bin);
        std::env::set_var("FDS_STATE", root.join("state.sqlite3"));
        std::env::remove_var("FDS_FORCE");
        std::env::remove_var("NO_COLOR");
        util::set_stop(false);
        util::set_tty(false);
        Self { root, bin, saved, _guard: guard }
    }

    // Return a path inside the sandbox.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    // Return the sandbox HOME directory.
    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    // Return the default scan state path as a string.
    pub fn state(&self) -> String {
        self.path("state.sqlite3").to_string_lossy().into_owned()
    }

    // Write a file (creating parent directories) and return its path.
    pub fn write(&self, rel: &str, data: &[u8]) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("create parent");
        fs::write(&p, data).expect("write file");
        p
    }

    // Create a directory and return its path.
    pub fn dir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).expect("create dir");
        p
    }

    // Install an executable shell script into the fake tool directory.
    pub fn tool(&self, name: &str, body: &str) {
        let p = self.bin.join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("write tool");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).expect("chmod tool");
    }
}

impl Drop for Sandbox {
    // Restore the environment and remove the directory.
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        util::set_stop(false);
        util::set_tty(false);
        let _ = fs::remove_dir_all(&self.root);
    }
}

// Tell whether the tests run as root, which bypasses permission checks.
pub fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

// Set a path's permission bits.
pub fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

// Set every file and directory under a path to a modification time in the year 2001.
pub fn age_tree(path: &Path) {
    let _ = util::set_times(path, 1_000_000_000_000_000_000, 1_000_000_000_000_000_000);
    let Ok(md) = fs::symlink_metadata(path) else {
        return;
    };
    if !md.is_dir() {
        return;
    }
    for entry in fs::read_dir(path).expect("read dir").flatten() {
        age_tree(&entry.path());
    }
}

// Create a git repository in a directory.
pub fn git_init(dir: &Path) {
    let status = std::process::Command::new("git").args(["init", "-q"]).arg(dir).status().expect("run git");
    assert!(status.success());
}

// Run a closure with process stdin replaced by the given text.
pub fn with_stdin<T>(text: &str, f: impl FnOnce() -> T) -> T {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe fills a two-element array; dup/dup2/close act on descriptors this function owns.
    unsafe {
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
        let saved = libc::dup(0);
        assert_eq!(libc::write(fds[1], text.as_ptr().cast(), text.len()), text.len() as isize);
        libc::close(fds[1]);
        libc::dup2(fds[0], 0);
        libc::close(fds[0]);
        let out = f();
        libc::dup2(saved, 0);
        libc::close(saved);
        out
    }
}
