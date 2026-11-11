// Cleanup of caches, Downloads, Trash, dev build artifacts, Homebrew and Docker.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::util::{self, c, hms, human_i, now};

const CACHE_DIRS: &[&str] = &[
    "~/.cache",
    "~/.nuget/packages",
    "~/Library/Caches",
    "~/Library/Application Support/Code/CachedExtensionVSIXs",
    "~/Library/Application Support/Code/CachedData",
    "~/Library/Application Support/Code/logs",
    "~/Library/Application Support/Claude/Cache",
    "~/Library/Application Support/Claude/Code Cache",
];
const XCODE_DIRS: &[&str] = &["~/Library/Developer/Xcode/DerivedData"];
const JS_LOCKS: &[&str] = &["package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "bun.lock"];
const DOTNET: &[&str] = &["*.csproj", "*.fsproj", "*.vbproj"];
const KEEP_TODAY: &str = "modified today (Atlantic/Reykjavik)";
const KEEP_IN_USE: &str = "in use by a running process/session";

// Rule proving an artifact directory is regenerable: sibling manifests, lockfiles and marker children.
struct Artifact {
    name: &'static str,
    manifests: &'static [&'static str],
    locks: &'static [&'static str],
    markers: &'static [&'static str],
}

const ARTIFACTS: &[Artifact] = &[
    Artifact { name: "node_modules", manifests: &["package.json"], locks: JS_LOCKS, markers: &[] },
    Artifact { name: ".next", manifests: &["package.json"], locks: &[], markers: &[] },
    Artifact { name: ".nuxt", manifests: &["package.json"], locks: &[], markers: &[] },
    Artifact { name: "dist", manifests: &["package.json"], locks: JS_LOCKS, markers: &[] },
    Artifact {
        name: "build",
        manifests: &["package.json", "build.gradle", "build.gradle.kts", "CMakeLists.txt", "setup.py"],
        locks: &[],
        markers: &[],
    },
    Artifact {
        name: "target",
        manifests: &["Cargo.toml", "pom.xml"],
        locks: &[],
        markers: &["CACHEDIR.TAG", ".rustc_info.json", "classes", "maven-status"],
    },
    Artifact {
        name: ".gradle",
        manifests: &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"],
        locks: &[],
        markers: &[],
    },
    Artifact { name: "__pycache__", manifests: &[], locks: &[], markers: &[] },
    Artifact { name: ".pytest_cache", manifests: &[], locks: &[], markers: &["CACHEDIR.TAG", "v"] },
    Artifact { name: ".mypy_cache", manifests: &[], locks: &[], markers: &["CACHEDIR.TAG"] },
    Artifact { name: ".ruff_cache", manifests: &[], locks: &[], markers: &["CACHEDIR.TAG"] },
    Artifact { name: "obj", manifests: DOTNET, locks: &[], markers: &[] },
    Artifact { name: "bin", manifests: DOTNET, locks: &[], markers: &[] },
    Artifact {
        name: "DerivedData",
        manifests: &["*.xcodeproj", "*.xcworkspace", "Package.swift"],
        locks: &[],
        markers: &[],
    },
];

// Totals and problem lists collected across cleanup targets.
#[derive(Default)]
pub struct Ctx {
    bytes: i64,
    protected: Vec<String>,
    failures: Vec<String>,
    uncertain: Vec<String>,
}

// Totals for one tree removal.
#[derive(Default)]
struct Removal {
    bytes: i64,
    scanned: u64,
    protected: Vec<String>,
}

// Describe an I/O error without the OS error number.
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    text.split(" (os error").next().unwrap_or_default().to_string()
}

// Return the first or last characters of text, up to a count.
fn clip(text: &str, n: usize, tail: bool) -> String {
    let chars: Vec<char> = text.trim().chars().collect();
    let range = if tail { chars.len().saturating_sub(n)..chars.len() } else { 0..n.min(chars.len()) };
    chars[range].iter().collect()
}

// Return allocated bytes, newest mtime and item count of a tree without following symlinks.
fn tree_size(path: &Path) -> (i64, f64, u64) {
    let (mut total, mut newest, mut n) = (0, 0.0f64, 0);
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&p) else {
            continue;
        };
        total += util::alloc(&md);
        newest = newest.max(md.mtime() as f64 + md.mtime_nsec() as f64 / 1e9);
        n += 1;
        if md.is_dir() {
            if let Ok(it) = fs::read_dir(&p) {
                stack.extend(it.flatten().map(|e| e.path()));
            }
        }
    }
    (total, newest, n)
}

// Delete a path recursively without following symlinks or changing permissions.
fn rm_tree(path: &Path, res: &mut Removal, tk: &mut util::Ticker, apply: bool) {
    let Ok(md) = fs::symlink_metadata(path) else {
        return;
    };
    res.scanned += 1;
    if md.is_dir() {
        let kids: Vec<PathBuf> = match fs::read_dir(path) {
            Ok(it) => it.flatten().map(|e| e.path()).collect(),
            Err(e) => {
                res.protected.push(format!("{} ({})", path.display(), strerror(&e)));
                return;
            }
        };
        for kid in kids {
            rm_tree(&kid, res, tk, apply);
            if util::stopped() {
                return;
            }
        }
        if apply {
            // Fails when a protected child remains, which is fine.
            let _ = fs::remove_dir(path);
        }
    } else {
        if apply {
            if let Err(e) = fs::remove_file(path) {
                res.protected.push(format!("{} ({})", path.display(), strerror(&e)));
                return;
            }
        }
        res.bytes += util::alloc(&md);
    }
    let verb = if apply { "removed" } else { "found" };
    tk.tick(&format!("  {verb} {} items, {} ...", res.scanned, human_i(res.bytes)), false);
}

// Remove a directory's contents but keep the directory itself.
fn empty_dir(path: &str, apply: bool, label: &str) -> Removal {
    let mut res = Removal::default();
    let path = PathBuf::from(util::expand(path));
    let is_real_dir = fs::symlink_metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
    if !is_real_dir {
        println!("  - {label}: not present");
        return res;
    }
    let kids: Vec<PathBuf> = match fs::read_dir(&path) {
        Ok(it) => it.flatten().map(|e| e.path()).collect(),
        Err(e) => {
            println!("  - {label}: cannot read ({})", strerror(&e));
            return res;
        }
    };
    let mut tk = util::Ticker::new();
    for kid in kids {
        rm_tree(&kid, &mut res, &mut tk, apply);
        if util::stopped() {
            break;
        }
    }
    tk.clear();
    let note = if res.protected.is_empty() {
        String::new()
    } else {
        format!("   ({} protected, skipped)", res.protected.len())
    };
    let mark = if apply { "-" } else { "~" };
    println!("  {mark} {label:<45} {:>10}{note}", human_i(res.bytes));
    res
}

// Empty each listed directory and accumulate totals.
fn clean_dirs(title: &str, dirs: &[&str], apply: bool, ctx: &mut Ctx) {
    println!("{}", c("1", title));
    for d in dirs {
        let r = empty_dir(d, apply, d);
        ctx.bytes += r.bytes;
        ctx.protected.extend(r.protected);
    }
}

// Empty the Trash through Finder.
fn clean_trash(apply: bool, ctx: &mut Ctx) {
    println!("{}", c("1", "Trash"));
    if !apply {
        ctx.bytes += empty_dir("~/.Trash", false, "~/.Trash (via Finder when applied)").bytes;
        return;
    }
    if !cfg!(target_os = "macos") {
        println!("  - skipped: Finder is macOS only");
        return;
    }
    let script = r#"tell application "Finder" to empty trash"#;
    let (code, out) = util::run_cmd(&["/usr/bin/osascript", "-e", script], "empty trash", 3600.0);
    if code == 0 {
        println!("  - Finder empty trash: ok");
        return;
    }
    println!("  - Finder empty trash: failed: {}", clip(&out, 200, false));
    ctx.failures.push(format!("empty trash: {}", clip(&out, 200, false)));
}

// Run Homebrew cleanup if installed.
fn clean_brew(apply: bool) {
    println!("{}", c("1", "Homebrew"));
    let Some(brew) = util::which("brew") else {
        println!("  - brew not installed");
        return;
    };
    if !apply {
        println!("  ~ would run: brew cleanup --prune=all -s");
        return;
    }
    let brew = brew.to_string_lossy();
    let (code, out) = util::run_cmd(&[&brew, "cleanup", "--prune=all", "-s"], "brew cleanup", 3600.0);
    let result = if code == 0 { "ok".to_string() } else { format!("failed: {}", clip(&out, 200, true)) };
    println!("  - brew cleanup: {result}");
}

// Tell whether a name matches an exact name or a leading-* suffix pattern.
fn name_matches(name: &str, pattern: &str) -> bool {
    match pattern.strip_prefix('*') {
        Some(suffix) => name.ends_with(suffix),
        None => name == pattern,
    }
}

// Tell whether the directory contains any entry matching one of the patterns.
fn has_any(parent: &Path, patterns: &[&str]) -> bool {
    let Ok(it) = fs::read_dir(parent) else {
        return false;
    };
    let names: Vec<String> = it.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    patterns.iter().any(|p| names.iter().any(|n| name_matches(n, p)))
}

// Return the timestamp of local midnight in Atlantic/Reykjavik, which is UTC+0 year-round.
fn today_start_ts() -> f64 {
    let t = now();
    t - t.rem_euclid(86400.0)
}

// Run a git command in a directory with optional locks disabled.
fn git(parent: &Path, args: &[&str]) -> Option<util::Output> {
    let dir = parent.to_string_lossy();
    let mut cmd = vec!["git", "-C", &dir];
    cmd.extend_from_slice(args);
    util::run(&cmd, 60.0, &[("GIT_OPTIONAL_LOCKS", "0")])
}

// Open files, working directories and command lines of running processes.
pub struct Snapshot {
    pub ok: bool,
    paths: Vec<String>,
    cwds: Vec<String>,
    cmds: Vec<String>,
}

impl Snapshot {
    // Capture the process snapshot via lsof and ps.
    pub fn capture() -> Self {
        let mut snap = Self { ok: false, paths: Vec::new(), cwds: Vec::new(), cmds: Vec::new() };
        let lsof = util::run(&["lsof", "-nP", "-F", "fn"], 180.0, &[]);
        let ps = util::run(&["ps", "-axo", "command="], 30.0, &[]);
        let (Some(lsof), Some(ps)) = (lsof, ps) else {
            return snap;
        };
        let mut cur = "";
        for line in lsof.stdout.lines() {
            if let Some(fd) = line.strip_prefix('f') {
                cur = fd;
            } else if line.starts_with("n/") {
                let list = if cur == "cwd" { &mut snap.cwds } else { &mut snap.paths };
                list.push(line[1..].to_string());
            }
        }
        for list in [&mut snap.paths, &mut snap.cwds] {
            list.sort();
            list.dedup();
        }
        snap.cmds = ps.stdout.lines().map(String::from).collect();
        snap.ok = !snap.paths.is_empty() || !snap.cwds.is_empty();
        snap
    }

    // Tell whether a sorted path list contains the root or anything beneath it.
    fn under(sorted: &[String], root: &str) -> bool {
        let i = sorted.partition_point(|p| p.as_str() < root);
        if sorted.get(i).is_some_and(|p| p == root) {
            return true;
        }
        let prefix = format!("{root}/");
        let j = sorted.partition_point(|p| p.as_str() < prefix.as_str());
        sorted.get(j).is_some_and(|p| p.starts_with(&prefix))
    }

    // Tell whether a process has this file or anything beneath it open.
    pub fn has_open(&self, path: &str) -> bool {
        Self::under(&self.paths, path)
    }

    // Tell whether a running process uses the candidate or its project directory.
    fn uses(&self, cand: &Path) -> bool {
        let cand_s = cand.to_string_lossy();
        let parent = cand.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let parent_slash = format!("{parent}/");
        Self::under(&self.paths, &cand_s)
            || Self::under(&self.cwds, &parent)
            || self.cmds.iter().any(|cmd| cmd.contains(&parent_slash) || cmd.ends_with(&parent))
    }
}

// Return Ok if a build artifact directory is safe to delete, else the reason it is kept.
fn verify_artifact(cand: &Path, cutoff: f64, snap: &Snapshot) -> Result<(), String> {
    let name = cand.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = cand.parent().unwrap_or(Path::new("/"));
    let is_real_dir = fs::symlink_metadata(cand).map(|m| m.is_dir()).unwrap_or(false);
    if !is_real_dir {
        return Err("not a real directory".into());
    }
    let rule = ARTIFACTS.iter().find(|a| a.name == name).ok_or("unknown artifact")?;
    if !rule.manifests.is_empty() && !has_any(parent, rule.manifests) {
        return Err(format!("no {} next to it", rule.manifests.join("/")));
    }
    if !rule.locks.is_empty() && !has_any(parent, rule.locks) {
        return Err("no lockfile - not provably regenerable".into());
    }
    if !rule.markers.is_empty() && !rule.markers.iter().any(|m| cand.join(m).exists()) {
        return Err("contents not recognised as generated".into());
    }
    if name == "__pycache__" {
        let all_pyc = fs::read_dir(cand).map(|it| {
            it.flatten().all(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.ends_with(".pyc") || n.ends_with(".pyo")
            })
        });
        if !all_pyc.unwrap_or(false) {
            return Err("contains non-.pyc file".into());
        }
    }
    if !git(parent, &["rev-parse", "--is-inside-work-tree"]).is_some_and(|o| o.code == 0) {
        return Err("not inside a git work tree".into());
    }
    if !git(parent, &["check-ignore", "-q", "--", &name]).is_some_and(|o| o.code == 0) {
        return Err("not ignored by git".into());
    }
    if !git(parent, &["ls-files", "-z", "--", &name]).is_some_and(|o| o.code == 0 && o.stdout.is_empty()) {
        return Err("git tracks files inside".into());
    }
    if tree_size(cand).1 >= cutoff {
        return Err(KEEP_TODAY.into());
    }
    if !snap.ok {
        return Err("could not snapshot running processes".into());
    }
    if snap.uses(cand) {
        return Err(KEEP_IN_USE.into());
    }
    Ok(())
}

// Collect known build-artifact directories under a root without descending into them.
fn find_artifacts(root: &Path, tk: &mut util::Ticker) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(it) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in it.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if ARTIFACTS.iter().any(|a| a.name == name) {
                found.push(entry.path());
                let msg =
                    format!("  found {} candidate directories, at {}", found.len(), entry.path().display());
                tk.tick(&msg, false);
            } else if name != ".git" {
                stack.push(entry.path());
            }
        }
    }
    found
}

// Delete verified, old, unused build artifacts under the dev root.
fn clean_dev(apply: bool, ctx: &mut Ctx, dev_root: &str) {
    println!("{}", c("1", &format!("Dev build artifacts under {dev_root}")));
    let root = PathBuf::from(util::realpath(dev_root));
    if !root.is_dir() {
        println!("  - not present");
        return;
    }
    if util::which("git").is_none() {
        println!("  - git not available; skipping");
        return;
    }
    let cutoff = today_start_ts();
    let day =
        chrono::DateTime::from_timestamp(cutoff as i64, 0).map(|d| d.format("%Y-%m-%d 00:00").to_string());
    println!("  keeping anything modified since {} (Atlantic/Reykjavik)", day.unwrap_or_default());
    println!("  snapshotting running processes ...");
    let snap = Snapshot::capture();
    let mut tk = util::Ticker::new();
    let cands = find_artifacts(&root, &mut tk);
    tk.clear();
    let mut kept = 0;
    for cand in &cands {
        if util::stopped() {
            break;
        }
        if let Err(why) = verify_artifact(cand, cutoff, &snap) {
            kept += 1;
            if why == KEEP_TODAY || why == KEEP_IN_USE {
                println!("  keep   {}  [{why}]", cand.display());
            } else {
                ctx.uncertain.push(format!("{} [{why}]", cand.display()));
            }
            continue;
        }
        if !apply {
            let size = tree_size(cand).0;
            println!("  ~ would delete {:>10}  {}", human_i(size), cand.display());
            ctx.bytes += size;
            continue;
        }
        // Recheck right before deleting.
        if let Err(why) = verify_artifact(cand, cutoff, &snap) {
            ctx.uncertain.push(format!("{} [changed before delete: {why}]", cand.display()));
            continue;
        }
        let mut res = Removal::default();
        rm_tree(cand, &mut res, &mut tk, true);
        tk.clear();
        ctx.bytes += res.bytes;
        let note = if res.protected.is_empty() {
            String::new()
        } else {
            format!("  ({} protected)", res.protected.len())
        };
        println!("  - deleted {:>10}  {}{note}", human_i(res.bytes), cand.display());
        ctx.protected.extend(res.protected);
    }
    println!("  {} candidates examined, {kept} kept/uncertain", cands.len());
}

// Tell whether Docker Desktop is already running and responding.
fn docker_running() -> bool {
    if util::which("docker").is_none() {
        return false;
    }
    let desktop = util::run(&["pgrep", "-fl", "Docker Desktop|com.docker.backend"], 30.0, &[]);
    if !desktop.is_some_and(|o| o.code == 0) {
        return false;
    }
    util::run(&["docker", "info", "--format", "{{.ServerVersion}}"], 20.0, &[]).is_some_and(|o| o.code == 0)
}

// Prune images and build cache, stop containers and quit Docker only if it is already running.
fn clean_docker(apply: bool, ctx: &mut Ctx, stop_timeout: u64) -> String {
    println!("{}", c("1", "Docker"));
    if !docker_running() {
        println!("  - Docker is not running; not starting it. Nothing done.");
        return "not running (left alone)".into();
    }
    if !apply {
        println!(
            "  ~ would: docker image prune -a -f; docker builder prune -a -f; stop all containers \
             ({stop_timeout}s grace); quit Docker Desktop. Containers and volumes are never deleted."
        );
        return "running (dry run)".into();
    }
    for (label, cmd) in [
        ("image prune", ["docker", "image", "prune", "-a", "-f"]),
        ("builder prune", ["docker", "builder", "prune", "-a", "-f"]),
    ] {
        let (code, out) = util::run_cmd(&cmd, label, 3600.0);
        let tail = out.trim().lines().last().unwrap_or_default().to_string();
        let shown = if code == 0 { tail } else { format!("failed: {}", clip(&out, 200, true)) };
        println!("  - {label}: {shown}");
        if code != 0 {
            ctx.failures.push(format!("docker {label}"));
        }
    }
    let ids: Vec<String> = util::run(&["docker", "ps", "-q"], 60.0, &[])
        .map(|o| o.stdout.split_whitespace().map(String::from).collect())
        .unwrap_or_default();
    if !ids.is_empty() {
        let grace = stop_timeout.to_string();
        let mut cmd = vec!["docker", "stop", "-t", &grace];
        cmd.extend(ids.iter().map(String::as_str));
        let label = format!("stopping {} containers", ids.len());
        let limit = (stop_timeout * ids.len() as u64 + 120) as f64;
        let (code, out) = util::run_cmd(&cmd, &label, limit);
        let shown = if code == 0 { "ok".to_string() } else { format!("FAILED {}", clip(&out, 200, true)) };
        println!("  - stopped {} containers gracefully: {shown}", ids.len());
        if code != 0 {
            ctx.failures.push("docker stop".into());
            return "still running (container stop failed; Docker left running)".into();
        }
    }
    let quit = r#"tell application "Docker" to quit"#;
    util::run_cmd(&["/usr/bin/osascript", "-e", quit], "quit Docker Desktop", 120.0);
    let deadline = now() + 180.0;
    while now() < deadline && docker_running() {
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    let state = if docker_running() {
        ctx.failures.push("docker did not shut down".into());
        "STILL RUNNING after quit request"
    } else {
        "shut down"
    };
    println!("  - Docker Desktop: {state}");
    state.into()
}

// Order the requested targets, expanding `all`.
fn ordered_targets(requested: &[String]) -> Vec<&'static str> {
    let order = ["caches", "xcode", "downloads", "trash", "dev", "brew", "docker"];
    let all = requested.iter().any(|t| t == "all");
    let wanted = |t: &str| {
        requested.iter().any(|r| r == t)
            || (all && ["caches", "downloads", "trash", "dev", "docker"].contains(&t))
    };
    order.into_iter().filter(|t| wanted(t)).collect()
}

// Print one summary list, truncated to 30 entries.
fn print_list(title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!("  {title} ({}):", items.len());
    for item in items.iter().take(30) {
        println!("    - {item}");
    }
    if items.len() > 30 {
        println!("    ... and {} more", items.len() - 30);
    }
}

// Run the selected cleanup targets and print a summary.
pub fn cmd_clean(targets: &[String], apply: bool, dev_root: &str, docker_stop_timeout: u64) {
    let targets = ordered_targets(targets);
    let mode = if apply { " - APPLYING" } else { " - DRY RUN (add --apply to delete)" };
    println!("{}{mode}", c("1", "free-disk-space clean"));
    if apply && targets.contains(&"downloads") {
        println!("{}", c("33", "Note: ~/Downloads will be emptied completely, including hidden files."));
    }
    util::install_sigint();
    let free0 = util::report_free("Initial");
    let mut ctx = Ctx::default();
    let mut docker_status = None;
    let t0 = now();
    for t in targets {
        if util::stopped() {
            break;
        }
        match t {
            "caches" => clean_dirs("Caches", CACHE_DIRS, apply, &mut ctx),
            "xcode" => clean_dirs("Xcode derived data", XCODE_DIRS, apply, &mut ctx),
            "downloads" => clean_dirs("Downloads", &["~/Downloads"], apply, &mut ctx),
            "trash" => clean_trash(apply, &mut ctx),
            "dev" => clean_dev(apply, &mut ctx, dev_root),
            "brew" => clean_brew(apply),
            _ => docker_status = Some(clean_docker(apply, &mut ctx, docker_stop_timeout)),
        }
        println!("  [elapsed {} | free now {}]", hms(now() - t0), human_i(util::free_bytes()));
    }
    let free1 = util::report_free("Final  ");
    println!("{}", c("1", "Summary"));
    let verb = if apply { "deleted" } else { "would delete" };
    println!("  {verb} (sum of file sizes seen): {}", human_i(ctx.bytes));
    if apply {
        println!(
            "  actual free-space change on volume: {}  (differs from the sum: snapshots, shared blocks, \
             concurrent writers)",
            human_i(free1 - free0)
        );
    }
    if let Some(state) = docker_status {
        println!("  docker: {state}");
    }
    print_list("protected / skipped entries", &ctx.protected);
    print_list("failures", &ctx.failures);
    print_list("uncertain, left untouched", &ctx.uncertain);
    if !apply {
        println!("Nothing was changed. Re-run with --apply.");
    }
    println!("Next: consider `new` + `compress` for transparent compression of inactive files.");
}

// Unit tests for cleanup helpers.
#[cfg(test)]
mod tests {
    use super::*;

    // Check that `all` expands to the default targets in fixed order.
    #[test]
    fn test_ordered_targets_all() {
        let req = vec!["brew".to_string(), "all".to_string()];
        assert_eq!(ordered_targets(&req), ["caches", "downloads", "trash", "dev", "brew", "docker"]);
    }

    // Check exact and suffix name patterns.
    #[test]
    fn test_name_matches_patterns() {
        assert!(name_matches("App.csproj", "*.csproj"));
        assert!(name_matches("package.json", "package.json"));
        assert!(!name_matches("package.json5", "package.json"));
    }

    // Check the sorted-prefix lookup used for open files.
    #[test]
    fn test_snapshot_under() {
        let list = vec!["/a/b".to_string(), "/a/bc".to_string(), "/x/y/z".to_string()];
        assert!(Snapshot::under(&list, "/a/b"));
        assert!(Snapshot::under(&list, "/x/y"));
        assert!(!Snapshot::under(&list, "/x/yy"));
    }

    // Check a dry-run tree removal counts files without deleting them.
    #[test]
    fn test_rm_tree_dry_run() {
        let dir = std::env::temp_dir().join(format!("fds-rm-{}", std::process::id()));
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/f"), vec![1u8; 10000]).unwrap();
        let mut res = Removal::default();
        rm_tree(&dir, &mut res, &mut util::Ticker::new(), false);
        assert!(dir.join("sub/f").exists());
        rm_tree(&dir, &mut Removal::default(), &mut util::Ticker::new(), true);
        assert!(!dir.exists());
        assert_eq!(res.scanned, 3);
    }
}
