// Verified in-place APFS compression of scanned candidates.
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rusqlite::params;

use crate::lzfse;
use crate::meta;
use crate::scan::{finished, open_db};
use crate::util::{self, c, die, hms, human_i, now, warn};

// Options for the compress command.
pub struct CompressOpts {
    pub state: String,
    pub sample: Option<usize>,
    pub max_files: Option<usize>,
    pub limit: Option<String>,
    pub min_age_days: i64,
    pub min_gain: i64,
    pub reserve: i64,
    pub dry_run: bool,
    pub yes: bool,
}

// A pending candidate as recorded by the scan.
struct Cand {
    path: String,
    size: i64,
    mtime_ns: i64,
    ino: i64,
    est: i64,
}

// Tell whether any process has the file open, treating failures as in use.
fn in_use(path: &str) -> bool {
    match util::run(&["/usr/sbin/lsof", "-t", "--", path], 60.0, &[]) {
        Some(out) => !out.stdout.trim().is_empty(),
        None => true,
    }
}

// Check that a candidate is still safe to compress and return a skip note if not; `allowed` flags do not block it.
fn precheck(cand: &Cand, md: &fs::Metadata, o: &CompressOpts, allowed: u32) -> Option<String> {
    if !md.is_file() || md.nlink() != 1 {
        return Some("not a plain single-link file".into());
    }
    if (md.len() as i64, util::mtime_ns(md), md.ino() as i64) != (cand.size, cand.mtime_ns, cand.ino) {
        return Some("changed since scan".into());
    }
    if util::flags(md) & util::NO_TOUCH_FLAGS & !allowed != 0 {
        return Some("already compressed / flagged".into());
    }
    if allowed & util::UF_COMPRESSED == 0 {
        if let Some(note) = meta::resource_fork_note(Path::new(&cand.path)) {
            return Some(note);
        }
    }
    if now() - (md.mtime() as f64) < o.min_age_days as f64 * 86400.0 {
        return Some("modified recently".into());
    }
    let dir = Path::new(&cand.path).parent().unwrap_or(Path::new("/"));
    if !util::access(dir, libc::W_OK) {
        return Some("directory not writable".into());
    }
    let need = md.len() as i64 + o.reserve;
    if util::free_bytes() < need {
        return Some(format!("not enough free space for temp copy (need {})", human_i(need)));
    }
    if in_use(&cand.path) {
        return Some("file is open by a process".into());
    }
    None
}

#[cfg(test)]
thread_local! {
    // Per-test switch that makes compressed_copy fail, standing in for a copy or library error.
    static FAIL_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// Make `tmp` a compressed copy of `path`: clone the file, give it the original's ACL, then put the LZFSE library's
// compressed form on it.
fn compressed_copy(path: &Path, tmp: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_COPY.with(std::cell::Cell::get) {
        return Err(std::io::Error::other("copy failed"));
    }
    util::clone_file(path, tmp)?;
    let source = fs::File::open(path)?;
    let target = fs::OpenOptions::new().read(true).write(true).open(tmp)?;
    // The clone's ACL does not always read back the same as the original's, so set it explicitly.
    meta::copy_acl(path, &target)?;
    lzfse::compress_onto(&source, &target)
}

// Make a compressed copy with `make`, verify it and swap it in; return (status, note, saved).
fn compress_verified(
    path: &Path,
    tmp: &Path,
    st: &fs::Metadata,
    o: &CompressOpts,
    make: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<(&'static str, String, i64)> {
    make(path, tmp)?;
    let tm = fs::symlink_metadata(tmp)?;
    let tmp_flags = util::flags(&tm);
    if tmp_flags & util::UF_COMPRESSED == 0 {
        return Ok(("skipped", "APFS declined to compress (no gain)".into(), 0));
    }
    let saved = util::alloc(st) - util::alloc(&tm);
    if saved < o.min_gain {
        return Ok(("skipped", format!("gain too small ({})", human_i(saved.max(0))), 0));
    }
    if tm.len() != st.len() {
        return Ok(("failed", "size mismatch".into(), 0));
    }
    let owner = |m: &fs::Metadata| (util::perm_bits(m), m.uid(), m.gid());
    if owner(&tm) != owner(st) {
        return Ok(("failed", "permissions/ownership differ".into(), 0));
    }
    if !meta::same_flags(util::flags(st), tmp_flags) {
        return Ok(("failed", "file flags differ".into(), 0));
    }
    let copied = meta::xattrs(tmp)?;
    if meta::xattrs(path)?.iter().any(|(name, value)| copied.get(name) != Some(value)) {
        return Ok(("failed", "extended attributes differ".into(), 0));
    }
    if meta::acl_text(path)? != meta::acl_text(tmp)? {
        return Ok(("failed", "ACL differs".into(), 0));
    }
    if util::sha256(path)? != util::sha256(tmp)? {
        return Ok(("failed", "content hash mismatch".into(), 0));
    }
    util::set_times(tmp, st.atime() * 1_000_000_000 + st.atime_nsec(), util::mtime_ns(st))?;
    // Set after the other times: macOS moves a birth time later than the modification time back to it.
    if let Some(born) = meta::birth_ns(st) {
        meta::set_birth(tmp, born)?;
    }
    if !meta::same_times(st, &fs::symlink_metadata(tmp)?) {
        return Ok(("failed", "times differ".into(), 0));
    }
    let st2 = fs::symlink_metadata(path)?;
    let ident = |m: &fs::Metadata| (m.len(), util::mtime_ns(m), m.ino(), m.nlink());
    if ident(&st2) != (st.len(), util::mtime_ns(st), st.ino(), 1) {
        return Ok(("skipped", "changed during compression".into(), 0));
    }
    fs::rename(tmp, path)?;
    Ok(("done", String::new(), saved))
}

// Compress one file in place via a verified temp copy and return (status, note, saved bytes).
fn compress_one(cand: &Cand, o: &CompressOpts) -> (&'static str, String, i64) {
    let path = Path::new(&cand.path);
    let st = match fs::symlink_metadata(path) {
        Ok(st) => st,
        Err(e) => return ("skipped", format!("gone: {e}"), 0),
    };
    if let Some(note) = precheck(cand, &st, o, 0) {
        return ("skipped", note, 0);
    }
    let tmp = util::temp_beside(path);
    let result = compress_verified(path, &tmp, &st, o, &compressed_copy);
    util::remove_temp(&tmp);
    result.unwrap_or_else(|e| ("failed", e.to_string(), 0))
}

// Replace a file that is flagged compressed but saves no space with a verified copy that `make` compresses for real.
pub fn rewrite_flagged(
    path: &Path,
    o: &CompressOpts,
    make: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> (&'static str, String, i64) {
    let st = match fs::symlink_metadata(path) {
        Ok(st) => st,
        Err(e) => return ("skipped", format!("gone: {e}"), 0),
    };
    let cand = Cand {
        path: path.to_string_lossy().into_owned(),
        size: st.len() as i64,
        mtime_ns: util::mtime_ns(&st),
        ino: st.ino() as i64,
        est: 0,
    };
    if let Some(note) = precheck(&cand, &st, o, util::UF_COMPRESSED) {
        return ("skipped", note, 0);
    }
    let tmp = util::temp_beside(path);
    let result = compress_verified(path, &tmp, &st, o, make);
    util::remove_temp(&tmp);
    result.unwrap_or_else(|e| ("failed", e.to_string(), 0))
}

// Load pending candidates up to the file and size limits.
fn plan(o: &CompressOpts) -> (rusqlite::Connection, Vec<Cand>, i64) {
    let db = open_db(&o.state, false);
    if !finished(&db) {
        warn("scan is not complete; compressing from the candidates found so far");
    }
    let limit_files = o.sample.or(o.max_files).unwrap_or(usize::MAX);
    let limit_bytes = o.limit.as_deref().map(|l| util::parse_size(l).unwrap_or_else(|e| die(&e)));
    let sql =
        "SELECT path,size,mtime_ns,ino,est_saved FROM cands WHERE status='pending' ORDER BY est_saved DESC";
    let rows: Vec<Cand> = {
        let mut stmt = db.prepare(sql).unwrap_or_else(|e| die(&e.to_string()));
        let it = stmt.query_map([], |r| {
            Ok(Cand { path: r.get(0)?, size: r.get(1)?, mtime_ns: r.get(2)?, ino: r.get(3)?, est: r.get(4)? })
        });
        it.map(|i| i.flatten().collect()).unwrap_or_default()
    };
    let (mut chosen, mut total) = (Vec::new(), 0);
    for cand in rows {
        if chosen.len() >= limit_files || limit_bytes.is_some_and(|l| total >= l) {
            break;
        }
        total += cand.est;
        chosen.push(cand);
    }
    (db, chosen, total)
}

// Ask the user to confirm on a terminal and return true to proceed.
fn confirm(question: &str) -> bool {
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().eq_ignore_ascii_case("y")
}

// Tell whether compressed copies work here: compress a probe file and read it back through the kernel.
pub fn compression_works() -> bool {
    let dir = std::env::temp_dir().join(format!("fds-probe-{}", std::process::id()));
    let (src, dst) = (dir.join("probe.txt"), dir.join("probe.out"));
    let text = "free-disk-space compression probe\n".repeat(8192);
    let ok = fs::create_dir_all(&dir).is_ok()
        && fs::write(&src, &text).is_ok()
        && compressed_copy(&src, &dst).is_ok()
        && fs::symlink_metadata(&dst).is_ok_and(|m| util::flags(&m) & util::UF_COMPRESSED != 0)
        && fs::read(&dst).is_ok_and(|back| back == text.as_bytes());
    let _ = fs::remove_dir_all(&dir);
    ok
}

// Group a skip note by its reason, dropping per-file details such as sizes and errors.
fn reason_key(note: &str) -> &str {
    note.split([':', '(']).next().unwrap_or(note).trim()
}

// Print how many files were skipped for each reason, most common first.
fn print_skip_reasons(reasons: &BTreeMap<String, usize>) {
    let mut rows: Vec<_> = reasons.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, n) in rows {
        println!("  skipped {n:>7}  {reason}");
    }
    if !reasons.is_empty() {
        println!("  (per file: free-disk-space list --status skipped)");
    }
}

// Compress planned candidates in place and report verified savings.
pub fn cmd_compress(o: &CompressOpts) {
    let forced = std::env::var_os("FDS_FORCE").is_some();
    if !cfg!(target_os = "macos") && !forced {
        die("compress requires macOS (APFS transparent compression)");
    }
    let (db, chosen, total) = plan(o);
    if chosen.is_empty() {
        println!("No pending candidates.");
        return;
    }
    let sample = if o.sample.is_some() { "  (SAMPLE run)" } else { "" };
    println!("Plan: {} files, ~{} estimated savings{sample}", chosen.len(), human_i(total));
    if o.dry_run {
        for cand in chosen.iter().take(25) {
            println!("  {:>10} ~{:>10} {}", human_i(cand.size), human_i(cand.est), cand.path);
        }
        println!("Dry run - nothing changed.");
        return;
    }
    if cfg!(target_os = "macos") && !compression_works() {
        die("a compressed test copy did not read back as compressed and unchanged; nothing was changed");
    }
    if util::is_tty() && !o.yes && !confirm("Compress these files in place? [y/N] ") {
        println!("Aborted.");
        return;
    }
    util::install_sigint();
    let free0 = util::report_free("Initial");
    let mut tk = util::Ticker::new();
    let (mut saved_total, mut n_done, mut n_skip, mut n_fail, mut streak) = (0i64, 0, 0, 0, 0);
    let mut reasons = BTreeMap::new();
    let t0 = now();
    for (i, cand) in chosen.iter().enumerate() {
        if util::stopped() {
            break;
        }
        let tail: String = cand.path.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
        tk.tick(
            &format!(
                "[{}/{}] compressed {n_done} | not compressed {} | {tail} | saved {} | {}",
                i + 1,
                chosen.len(),
                n_skip + n_fail,
                human_i(saved_total),
                hms(now() - t0)
            ),
            true,
        );
        let (status, note, saved) = compress_one(cand, o);
        tk.clear();
        let _ = db.execute(
            "UPDATE cands SET status=?, note=?, saved=? WHERE path=?",
            params![status, note, saved, cand.path],
        );
        match status {
            "done" => {
                n_done += 1;
                saved_total += saved;
                streak = 0;
            }
            "skipped" => {
                n_skip += 1;
                *reasons.entry(reason_key(&note).to_string()).or_insert(0) += 1;
            }
            _ => {
                n_fail += 1;
                streak += 1;
                println!("{} {}: {note}", c("31", "FAILED"), cand.path);
                if streak >= 3 {
                    println!("{}", c("31", "3 consecutive failures - aborting for safety."));
                    break;
                }
            }
        }
        if !util::is_tty() && now() - tk.last >= 15.0 {
            let msg = format!(
                "progress: {}/{} files, compressed {n_done}, not compressed {}, verified saved {}",
                i + 1,
                chosen.len(),
                n_skip + n_fail,
                human_i(saved_total)
            );
            tk.tick(&msg, true);
        }
    }
    let free1 = util::report_free("Final");
    println!("Compressed {n_done}, skipped {n_skip}, failed {n_fail} in {}", hms(now() - t0));
    print_skip_reasons(&reasons);
    println!("Verified allocated-space saving (sum over files): {}", c("1", &human_i(saved_total)));
    println!("Overall free-space change on the volume:         {}", c("1", &human_i(free1 - free0)));
    println!("(These can differ: snapshots, shared blocks/clones and other writers affect free space.)");
    if o.sample.is_some() {
        println!("Sample finished. If it looks right, run `compress` without --sample.");
    }
}

// Unit tests for verified compression.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{age_tree, chmod, is_root, set_xattr, with_stdin, Sandbox};
    use std::path::PathBuf;

    // Build compress options for the sandbox state with permissive limits.
    fn opts(sb: &Sandbox) -> CompressOpts {
        CompressOpts {
            state: sb.state(),
            sample: None,
            max_files: None,
            limit: None,
            min_age_days: 14,
            min_gain: -1_000_000,
            reserve: 0,
            dry_run: false,
            yes: true,
        }
    }

    // Describe an existing file as a scan candidate.
    fn cand(path: &Path) -> Cand {
        let md = fs::symlink_metadata(path).unwrap();
        Cand {
            path: path.to_string_lossy().into_owned(),
            size: md.len() as i64,
            mtime_ns: util::mtime_ns(&md),
            ino: md.ino() as i64,
            est: md.len() as i64 / 2,
        }
    }

    // Write aged files and record them as pending candidates, largest estimate first.
    fn seed(sb: &Sandbox, names: &[&str]) -> Vec<PathBuf> {
        let db = open_db(&sb.state(), true);
        let mut paths = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let path = sb.write(&format!("data/{name}"), &vec![b'a'; 4096 * (names.len() - i + 1)]);
            age_tree(&path);
            let c = cand(&path);
            db.execute(
                "INSERT INTO cands(path,size,alloc,est_saved,mtime_ns,ino) VALUES(?,?,?,?,?,?)",
                params![c.path, c.size, c.size, c.est, c.mtime_ns, c.ino],
            )
            .unwrap();
            paths.push(path);
        }
        paths
    }

    // Make every compressed copy in this test thread fail, or work again.
    fn fail_copies(on: bool) {
        FAIL_COPY.with(|f| f.set(on));
    }

    #[cfg(not(target_os = "macos"))]
    // Build a copy step that copies the file and then changes the copy with `after`.
    fn copy_then(after: impl Fn(&Path) + 'static) -> impl Fn(&Path, &Path) -> std::io::Result<()> {
        move |src: &Path, dst: &Path| {
            fs::copy(src, dst)?;
            after(dst);
            Ok(())
        }
    }

    // Read a candidate's recorded status and note.
    fn status_of(sb: &Sandbox, path: &Path) -> (String, String, i64) {
        let db = open_db(&sb.state(), false);
        db.query_row(
            "SELECT status, COALESCE(note,''), saved FROM cands WHERE path=?",
            [path.to_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    // Check the open-file probe reads lsof output and treats failures as in use.
    #[test]
    fn test_in_use_probe() {
        let sb = Sandbox::new();
        sb.tool("lsof", "exit 1");
        assert!(!in_use("/x"));
        sb.tool("lsof", "echo 123");
        assert!(in_use("/x"));
        std::fs::remove_file(sb.bin.join("lsof")).unwrap();
        assert!(in_use("/x"));
    }

    // Check every reason a candidate is skipped before compression.
    #[test]
    fn test_precheck_reasons() {
        let sb = Sandbox::new();
        sb.tool("lsof", "exit 1");
        let file = seed(&sb, &["a"]).remove(0);
        let o = opts(&sb);
        let check = |c: &Cand, o: &CompressOpts| precheck(c, &fs::symlink_metadata(&c.path).unwrap(), o, 0);
        let good = cand(&file);
        assert_eq!(check(&good, &o), None);
        let dir = sb.dir("data/adir");
        assert_eq!(check(&cand(&dir), &o).unwrap(), "not a plain single-link file");
        fs::hard_link(&file, sb.path("data/link")).unwrap();
        assert_eq!(check(&good, &o).unwrap(), "not a plain single-link file");
        fs::remove_file(sb.path("data/link")).unwrap();
        assert_eq!(check(&Cand { size: 1, ..cand(&file) }, &o).unwrap(), "changed since scan");
        #[cfg(not(target_os = "macos"))]
        {
            util::set_flags(&[util::UF_COMPRESSED]);
            assert_eq!(check(&good, &o).unwrap(), "already compressed / flagged");
            let md = fs::symlink_metadata(&file).unwrap();
            assert_eq!(precheck(&good, &md, &o, util::UF_COMPRESSED), None);
            util::set_flags(&[]);
        }
        let recent = CompressOpts { min_age_days: 100_000, ..opts(&sb) };
        assert_eq!(check(&good, &recent).unwrap(), "modified recently");
        let greedy = CompressOpts { reserve: i64::MAX / 2, ..opts(&sb) };
        assert!(check(&good, &greedy).unwrap().starts_with("not enough free space"));
        sb.tool("lsof", "echo 1");
        assert_eq!(check(&good, &o).unwrap(), "file is open by a process");
        assert_eq!(meta::resource_fork_note(&file), None);
        assert!(meta::resource_fork_note(&sb.path("missing"))
            .unwrap()
            .starts_with("cannot read resource fork"));
        #[cfg(target_os = "macos")]
        {
            sb.tool("lsof", "exit 1");
            // Writing the fork changes the modification time, so age the file again and describe it afresh.
            meta::set_xattr(&file, "com.apple.ResourceFork", b"an old resource fork").unwrap();
            age_tree(&file);
            let forked = cand(&file);
            let md = fs::symlink_metadata(&file).unwrap();
            assert_eq!(precheck(&forked, &md, &o, 0).unwrap(), "has a resource fork");
            assert_eq!(precheck(&forked, &md, &o, util::UF_COMPRESSED), None);
        }
    }

    // Check a candidate in an unwritable directory is skipped.
    #[test]
    fn test_precheck_unwritable_dir() {
        let sb = Sandbox::new();
        if is_root() {
            return;
        }
        let file = seed(&sb, &["a"]).remove(0);
        chmod(&sb.path("data"), 0o555);
        let md = fs::symlink_metadata(&file).unwrap();
        assert_eq!(precheck(&cand(&file), &md, &opts(&sb), 0).unwrap(), "directory not writable");
        chmod(&sb.path("data"), 0o755);
    }

    // Check the verified copy reports each failure and succeeds when everything matches.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_compress_verified_outcomes() {
        let sb = Sandbox::new();
        let file = seed(&sb, &["a"]).remove(0);
        chmod(&file, 0o644);
        let tmp = util::temp_beside(&file);
        let o = opts(&sb);
        let run = |o: &CompressOpts, make: &dyn Fn(&Path, &Path) -> std::io::Result<()>| {
            let st = fs::symlink_metadata(&file).unwrap();
            let r = compress_verified(&file, &tmp, &st, o, make);
            util::remove_temp(&tmp);
            r
        };
        let broken =
            |_: &Path, _: &Path| Err(std::io::Error::other("lzfse: setting com.apple.decmpfs: denied"));
        assert!(run(&o, &broken).unwrap_err().to_string().contains("setting com.apple.decmpfs"));
        let plain = copy_then(|_| ());
        util::set_flags(&[0]);
        assert_eq!(run(&o, &plain).unwrap().0, "skipped");
        util::set_flags(&[util::UF_COMPRESSED]);
        let stingy = CompressOpts { min_gain: 1 << 40, ..opts(&sb) };
        assert!(run(&stingy, &plain).unwrap().1.starts_with("gain too small"));
        let short = copy_then(|d| fs::write(d, b"short").unwrap());
        assert_eq!(run(&o, &short).unwrap().1, "size mismatch");
        let private = copy_then(|d| chmod(d, 0o600));
        assert_eq!(run(&o, &private).unwrap().1, "permissions/ownership differ");
        util::set_flags(&[util::UF_COMPRESSED, 0x8000]);
        assert_eq!(run(&o, &plain).unwrap().1, "file flags differ");
        util::set_flags(&[util::UF_COMPRESSED]);
        let len = fs::metadata(&file).unwrap().len() as usize;
        let garbled = copy_then(move |d| fs::write(d, vec![b'z'; len]).unwrap());
        assert_eq!(run(&o, &garbled).unwrap().1, "content hash mismatch");
        let stale = fs::symlink_metadata(&file).unwrap();
        util::set_times(&file, 1_000_000_000_000_000_000, 1_100_000_000_000_000_000).unwrap();
        let r = compress_verified(&file, &tmp, &stale, &o, &plain).unwrap();
        util::remove_temp(&tmp);
        assert_eq!(r.1, "changed during compression");
        util::set_times(&file, 1_000_000_000_000_000_000, 1_000_000_000_000_000_000).unwrap();
        let (status, note, saved) = run(&o, &plain).unwrap();
        assert_eq!((status, note.as_str()), ("done", ""));
        assert!(saved >= 0);
    }

    // Check an extended attribute whose value the copy does not keep fails verification, and one it keeps passes.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_compress_verified_xattr_values() {
        let sb = Sandbox::new();
        let file = seed(&sb, &["a"]).remove(0);
        chmod(&file, 0o644);
        if !set_xattr(&file, "user.keep", b"yes") {
            return;
        }
        let tmp = util::temp_beside(&file);
        let run = |value: &'static [u8]| {
            let st = fs::symlink_metadata(&file).unwrap();
            let make = copy_then(move |d| assert!(set_xattr(d, "user.keep", value)));
            let r = compress_verified(&file, &tmp, &st, &opts(&sb), &make).unwrap();
            util::remove_temp(&tmp);
            r
        };
        util::set_flags(&[util::UF_COMPRESSED]);
        assert_eq!(run(b"no").1, "extended attributes differ");
        assert_eq!(run(b"yes").0, "done");
    }

    // Check on macOS that a real compressed replacement keeps an ACL, an attribute value and the birth time, and that
    // a copy losing the ACL or changing the value fails verification.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_compress_keeps_macos_metadata() {
        let sb = Sandbox::new();
        let text = b"metadata must survive compression. ".repeat(4000);
        let file = sb.write("data/m", &text);
        chmod(&file, 0o644);
        assert!(set_xattr(&file, "com.example.tag", b"blue"));
        let acl = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow read")
            .arg(&file)
            .status()
            .unwrap();
        assert!(acl.success());
        let hidden = 0x8000;
        let flags = util::flags(&fs::symlink_metadata(&file).unwrap());
        meta::set_flags(&fs::File::open(&file).unwrap(), flags | hidden).unwrap();
        let born = 1_000_000_000_000_000_000;
        meta::set_birth(&file, born).unwrap();
        let mtime = util::mtime_ns(&fs::symlink_metadata(&file).unwrap());
        let tmp = util::temp_beside(&file);
        let o = CompressOpts { min_gain: 0, ..opts(&sb) };
        let run = |make: &dyn Fn(&Path, &Path) -> std::io::Result<()>| {
            let st = fs::symlink_metadata(&file).unwrap();
            let r = compress_verified(&file, &tmp, &st, &o, make).unwrap();
            util::remove_temp(&tmp);
            r
        };
        let unprotected = |s: &Path, d: &Path| {
            compressed_copy(s, d)?;
            assert!(std::process::Command::new("/bin/chmod").arg("-N").arg(d).status()?.success());
            Ok(())
        };
        assert_eq!(run(&unprotected).1, "ACL differs");
        let retagged = |s: &Path, d: &Path| {
            compressed_copy(s, d)?;
            assert!(set_xattr(d, "com.example.tag", b"red"));
            Ok(())
        };
        assert_eq!(run(&retagged).1, "extended attributes differ");
        let shown = |s: &Path, d: &Path| {
            compressed_copy(s, d)?;
            let target = fs::File::open(d)?;
            meta::set_flags(&target, util::flags(&target.metadata()?) & !hidden)
        };
        assert_eq!(run(&shown).1, "file flags differ");
        let before = fs::symlink_metadata(&file).unwrap();
        let (status, note, _) = run(&compressed_copy);
        assert_eq!((status, note.as_str()), ("done", ""));
        let md = fs::symlink_metadata(&file).unwrap();
        assert_ne!(util::flags(&md) & util::UF_COMPRESSED, 0);
        assert_eq!(fs::read(&file).unwrap(), text);
        assert_eq!(
            meta::xattrs(&file).unwrap().get("com.example.tag").map(Vec::as_slice),
            Some(&b"blue"[..])
        );
        assert!(meta::acl_text(&file).unwrap().unwrap().contains("allow"));
        assert_eq!(meta::birth_ns(&md), Some(born));
        assert_eq!(util::mtime_ns(&md), mtime);
        assert!(meta::same_times(&before, &md));
        assert_ne!(util::flags(&md) & hidden, 0);
    }

    // Check the real copy step clones the file and runs it through the library, keeping the contents.
    #[test]
    fn test_compressed_copy_keeps_contents() {
        let sb = Sandbox::new();
        let text = b"compress me through the library, please. ".repeat(3000);
        let file = sb.write("data/x", &text);
        let tmp = util::temp_beside(&file);
        compressed_copy(&file, &tmp).unwrap();
        assert_eq!(fs::read(&tmp).unwrap(), text);
        util::remove_temp(&tmp);
        fail_copies(true);
        assert!(compressed_copy(&file, &tmp).is_err());
        fail_copies(false);
    }

    // Check compress_one handles a vanished file, a skip, a failure and a success.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_compress_one_outcomes() {
        let sb = Sandbox::new();
        sb.tool("lsof", "exit 1");
        let file = seed(&sb, &["a"]).remove(0);
        let o = opts(&sb);
        let missing = Cand { path: sb.path("gone").to_string_lossy().into_owned(), ..cand(&file) };
        assert_eq!(compress_one(&missing, &o).0, "skipped");
        assert_eq!(compress_one(&Cand { size: 1, ..cand(&file) }, &o).1, "changed since scan");
        fail_copies(true);
        util::set_flags(&[0, util::UF_COMPRESSED]);
        assert_eq!(compress_one(&cand(&file), &o), ("failed", "copy failed".to_string(), 0));
        fail_copies(false);
        chmod(&file, 0o644);
        util::set_flags(&[0, util::UF_COMPRESSED]);
        assert_eq!(compress_one(&cand(&file), &o).0, "done");
    }

    // Check candidate planning honours file and size limits and warns about unfinished scans.
    #[test]
    fn test_plan_limits() {
        let sb = Sandbox::new();
        seed(&sb, &["a", "b", "c"]);
        let (_db, all, total) = plan(&opts(&sb));
        assert_eq!(all.len(), 3);
        assert_eq!(total, all.iter().map(|c| c.est).sum::<i64>());
        assert!(all[0].est >= all[1].est);
        let (_db, some, _) = plan(&CompressOpts { sample: Some(2), ..opts(&sb) });
        assert_eq!(some.len(), 2);
        let (_db, some, _) = plan(&CompressOpts { max_files: Some(1), ..opts(&sb) });
        assert_eq!(some.len(), 1);
        let (_db, some, _) = plan(&CompressOpts { limit: Some("1".into()), ..opts(&sb) });
        assert_eq!(some.len(), 1);
        let bad = CompressOpts { limit: Some("lots".into()), ..opts(&sb) };
        assert!(std::panic::catch_unwind(|| plan(&bad)).is_err());
        let missing = CompressOpts { state: sb.path("none").to_string_lossy().into_owned(), ..opts(&sb) };
        assert!(std::panic::catch_unwind(|| plan(&missing)).is_err());
    }

    // Check the confirmation prompt accepts only a yes answer.
    #[test]
    fn test_confirm_answers() {
        let _sb = Sandbox::new();
        assert!(with_stdin("y\n", || confirm("ok? ")));
        assert!(with_stdin("  Y  \n", || confirm("ok? ")));
        assert!(!with_stdin("n\n", || confirm("ok? ")));
        assert!(!with_stdin("", || confirm("ok? ")));
    }

    // Check the compress command refuses to run off macOS without the override.
    #[test]
    fn test_cmd_compress_requires_macos() {
        let sb = Sandbox::new();
        let o = opts(&sb);
        if cfg!(target_os = "macos") {
            return;
        }
        assert!(std::panic::catch_unwind(|| cmd_compress(&o)).is_err());
    }

    // Check the compress command with no candidates, a dry run, a declined prompt and a sample note.
    #[test]
    fn test_cmd_compress_planning_modes() {
        let sb = Sandbox::new();
        std::env::set_var("FDS_FORCE", "1");
        if cfg!(target_os = "macos") {
            return;
        }
        drop(open_db(&sb.state(), true));
        cmd_compress(&opts(&sb));
        seed(&sb, &["a", "b"]);
        let dry = CompressOpts { dry_run: true, sample: Some(1), ..opts(&sb) };
        cmd_compress(&dry);
        util::set_tty(true);
        let ask = CompressOpts { yes: false, ..opts(&sb) };
        with_stdin("n\n", || cmd_compress(&ask));
        assert_eq!(status_of(&sb, &sb.path("data/a")).0, "pending");
        fail_copies(true);
        sb.tool("lsof", "exit 1");
        with_stdin("y\n", || cmd_compress(&CompressOpts { sample: Some(1), yes: false, ..opts(&sb) }));
        assert_eq!(status_of(&sb, &sb.path("data/a")).0, "failed");
        fail_copies(false);
    }

    // Check a full compress run records done, skipped and failed files and aborts after repeated failures.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_cmd_compress_run() {
        let sb = Sandbox::new();
        std::env::set_var("FDS_FORCE", "1");
        sb.tool("lsof", "exit 1");
        let files = seed(&sb, &["a", "b", "c", "d", "e", "f", "g"]);
        chmod(&files[0], 0o644);
        chmod(&files[1], 0o644);
        fs::hard_link(&files[1], sb.path("data/b-link")).unwrap();
        util::set_flags(&[0, util::UF_COMPRESSED]);
        let o = CompressOpts { sample: Some(2), ..opts(&sb) };
        cmd_compress(&o);
        assert_eq!(status_of(&sb, &files[0]).0, "done");
        assert_eq!(status_of(&sb, &files[1]).0, "skipped");
        fail_copies(true);
        util::set_flags(&[0]);
        cmd_compress(&opts(&sb));
        let failed = files[2..].iter().filter(|f| status_of(&sb, f).0 == "failed").count();
        assert_eq!(failed, 3);
        assert_eq!(status_of(&sb, &files[5]).0, "pending");
        util::set_stop(true);
        cmd_compress(&opts(&sb));
        assert_eq!(status_of(&sb, &files[5]).0, "pending");
        fail_copies(false);
    }

    // Check skip notes group by reason without per-file details.
    #[test]
    fn test_reason_key_groups() {
        assert_eq!(reason_key("gain too small (12 KiB)"), "gain too small");
        assert_eq!(reason_key("gone: No such file"), "gone");
        assert_eq!(reason_key("modified recently"), "modified recently");
        let mut r = BTreeMap::new();
        r.insert("a".to_string(), 1);
        r.insert("b".to_string(), 3);
        print_skip_reasons(&r);
        print_skip_reasons(&BTreeMap::new());
    }

    // Check the probe needs a working compressed copy that reads as compressed.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_compression_works_probe() {
        let _sb = Sandbox::new();
        util::set_flags(&[util::UF_COMPRESSED]);
        assert!(compression_works());
        util::set_flags(&[0]);
        assert!(!compression_works());
        util::set_flags(&[util::UF_COMPRESSED]);
        fail_copies(true);
        assert!(!compression_works());
        fail_copies(false);
        util::set_flags(&[]);
    }
}
