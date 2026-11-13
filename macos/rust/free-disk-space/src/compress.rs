// Verified in-place APFS compression of scanned candidates.
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rusqlite::params;

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

// Check that a candidate is still safe to compress and return a skip note if not.
fn precheck(cand: &Cand, md: &fs::Metadata, o: &CompressOpts) -> Option<String> {
    if !md.is_file() || md.nlink() != 1 {
        return Some("not a plain single-link file".into());
    }
    if (md.len() as i64, util::mtime_ns(md), md.ino() as i64) != (cand.size, cand.mtime_ns, cand.ino) {
        return Some("changed since scan".into());
    }
    if util::flags(md) & util::NO_TOUCH_FLAGS != 0 {
        return Some("already compressed / flagged".into());
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

// Copy with compression to the temp file, verify it and swap it in; return (status, note, saved).
fn compress_verified(
    path: &Path,
    tmp: &Path,
    st: &fs::Metadata,
    o: &CompressOpts,
) -> std::io::Result<(&'static str, String, i64)> {
    let (p, t) = (path.to_string_lossy(), tmp.to_string_lossy());
    let out = util::run(&["/usr/bin/ditto", "--hfsCompression", &p, &t], 3600.0, &[])
        .ok_or_else(|| std::io::Error::other("cannot run ditto"))?;
    if out.code != 0 {
        let msg: String = out.stderr.trim().chars().take(200).collect();
        return Ok(("failed", format!("ditto: {msg}"), 0));
    }
    let tm = fs::symlink_metadata(tmp)?;
    if util::flags(&tm) & util::UF_COMPRESSED == 0 {
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
    if !util::xattr_names(path)?.is_subset(&util::xattr_names(tmp)?) {
        return Ok(("failed", "extended attributes lost".into(), 0));
    }
    if util::sha256(path)? != util::sha256(tmp)? {
        return Ok(("failed", "content hash mismatch".into(), 0));
    }
    util::set_times(tmp, st.atime() * 1_000_000_000 + st.atime_nsec(), util::mtime_ns(st))?;
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
    if let Some(note) = precheck(cand, &st, o) {
        return ("skipped", note, 0);
    }
    let tmp = util::temp_beside(path);
    let result = compress_verified(path, &tmp, &st, o);
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
        die("compress requires macOS (uses /usr/bin/ditto and APFS compression)");
    }
    if cfg!(target_os = "macos") && !Path::new("/usr/bin/ditto").exists() {
        die("/usr/bin/ditto not found - cannot compress; nothing was changed");
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
    use crate::testkit::{age_tree, chmod, is_root, with_stdin, Sandbox};
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

    #[cfg(not(target_os = "macos"))]
    // Install a fake ditto that copies the file and then runs an extra shell step on the copy.
    fn fake_ditto(sb: &Sandbox, after: &str) {
        sb.tool("ditto", &format!("cp \"$2\" \"$3\" || exit 1\n{after}"));
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
        let check = |c: &Cand, o: &CompressOpts| precheck(c, &fs::symlink_metadata(&c.path).unwrap(), o);
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
            util::set_flags(&[]);
        }
        let recent = CompressOpts { min_age_days: 100_000, ..opts(&sb) };
        assert_eq!(check(&good, &recent).unwrap(), "modified recently");
        let greedy = CompressOpts { reserve: i64::MAX / 2, ..opts(&sb) };
        assert!(check(&good, &greedy).unwrap().starts_with("not enough free space"));
        sb.tool("lsof", "echo 1");
        assert_eq!(check(&good, &o).unwrap(), "file is open by a process");
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
        assert_eq!(precheck(&cand(&file), &md, &opts(&sb)).unwrap(), "directory not writable");
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
        let run = |o: &CompressOpts| {
            let st = fs::symlink_metadata(&file).unwrap();
            let r = compress_verified(&file, &tmp, &st, o);
            util::remove_temp(&tmp);
            r
        };
        std::env::set_var("PATH", &sb.bin);
        assert!(run(&o).unwrap_err().to_string().contains("cannot run ditto"));
        std::env::set_var("PATH", format!("{}:/usr/bin:/bin", sb.bin.display()));
        fake_ditto(&sb, "echo oops >&2; exit 1");
        assert_eq!(run(&o).unwrap().0, "failed");
        sb.tool("ditto", "echo 'bad thing' >&2; exit 4");
        assert_eq!(run(&o).unwrap().1, "ditto: bad thing");
        fake_ditto(&sb, "true");
        util::set_flags(&[0]);
        assert_eq!(run(&o).unwrap().0, "skipped");
        util::set_flags(&[util::UF_COMPRESSED]);
        let stingy = CompressOpts { min_gain: 1 << 40, ..opts(&sb) };
        assert!(run(&stingy).unwrap().1.starts_with("gain too small"));
        fake_ditto(&sb, "head -c 10 \"$2\" > \"$3\"");
        assert_eq!(run(&o).unwrap().1, "size mismatch");
        fake_ditto(&sb, "chmod 600 \"$3\"");
        assert_eq!(run(&o).unwrap().1, "permissions/ownership differ");
        fake_ditto(&sb, "chmod 644 \"$3\"");
        sb.tool("xattr", "case \"$1\" in *.tmp) ;; *) echo user.keep;; esac");
        assert_eq!(run(&o).unwrap().1, "extended attributes lost");
        sb.tool("xattr", "exit 0");
        let len = fs::metadata(&file).unwrap().len() as usize;
        fake_ditto(&sb, &format!("head -c {len} /dev/zero | tr '\\0' 'z' > \"$3\"; chmod 644 \"$3\""));
        assert_eq!(run(&o).unwrap().1, "content hash mismatch");
        fake_ditto(&sb, "chmod 644 \"$3\"");
        let stale = fs::symlink_metadata(&file).unwrap();
        util::set_times(&file, 1_000_000_000_000_000_000, 1_100_000_000_000_000_000).unwrap();
        let r = compress_verified(&file, &tmp, &stale, &o).unwrap();
        util::remove_temp(&tmp);
        assert_eq!(r.1, "changed during compression");
        util::set_times(&file, 1_000_000_000_000_000_000, 1_000_000_000_000_000_000).unwrap();
        let (status, note, saved) = run(&o).unwrap();
        assert_eq!((status, note.as_str()), ("done", ""));
        assert!(saved >= 0);
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
        fake_ditto(&sb, "exit 1");
        util::set_flags(&[0, util::UF_COMPRESSED]);
        assert_eq!(compress_one(&cand(&file), &o).0, "failed");
        fake_ditto(&sb, "chmod 644 \"$3\"");
        chmod(&file, 0o644);
        sb.tool("xattr", "exit 0");
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
        sb.tool("ditto", "exit 1");
        sb.tool("lsof", "exit 1");
        with_stdin("y\n", || cmd_compress(&CompressOpts { sample: Some(1), yes: false, ..opts(&sb) }));
        assert_eq!(status_of(&sb, &sb.path("data/a")).0, "failed");
    }

    // Check a full compress run records done, skipped and failed files and aborts after repeated failures.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_cmd_compress_run() {
        let sb = Sandbox::new();
        std::env::set_var("FDS_FORCE", "1");
        sb.tool("lsof", "exit 1");
        sb.tool("xattr", "exit 0");
        let files = seed(&sb, &["a", "b", "c", "d", "e", "f", "g"]);
        chmod(&files[0], 0o644);
        chmod(&files[1], 0o644);
        fake_ditto(&sb, "chmod 644 \"$3\"");
        fs::hard_link(&files[1], sb.path("data/b-link")).unwrap();
        util::set_flags(&[0, util::UF_COMPRESSED]);
        let o = CompressOpts { sample: Some(2), ..opts(&sb) };
        cmd_compress(&o);
        assert_eq!(status_of(&sb, &files[0]).0, "done");
        assert_eq!(status_of(&sb, &files[1]).0, "skipped");
        fake_ditto(&sb, "exit 1");
        util::set_flags(&[0]);
        cmd_compress(&opts(&sb));
        let failed = files[2..].iter().filter(|f| status_of(&sb, f).0 == "failed").count();
        assert_eq!(failed, 3);
        assert_eq!(status_of(&sb, &files[5]).0, "pending");
        util::set_stop(true);
        cmd_compress(&opts(&sb));
        assert_eq!(status_of(&sb, &files[5]).0, "pending");
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
}
