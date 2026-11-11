// Verified in-place APFS compression of scanned candidates.
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
    let t0 = now();
    for (i, cand) in chosen.iter().enumerate() {
        if util::stopped() {
            break;
        }
        let tail: String = cand.path.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
        tk.tick(
            &format!(
                "[{}/{}] {tail} | saved {} | {}",
                i + 1,
                chosen.len(),
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
                println!("{} {:<10} {}", c("32", "done  "), format!("-{}", human_i(saved)), cand.path);
            }
            "skipped" => {
                n_skip += 1;
                println!("{} {}: {note}", c("2", "skip  "), cand.path);
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
                "progress: {}/{} files, verified saved {}",
                i + 1,
                chosen.len(),
                human_i(saved_total)
            );
            tk.tick(&msg, true);
        }
    }
    let free1 = util::report_free("Final");
    println!("Compressed {n_done}, skipped {n_skip}, failed {n_fail} in {}", hms(now() - t0));
    println!("Verified allocated-space saving (sum over files): {}", c("1", &human_i(saved_total)));
    println!("Overall free-space change on the volume:         {}", c("1", &human_i(free1 - free0)));
    println!("(These can differ: snapshots, shared blocks/clones and other writers affect free space.)");
    if o.sample.is_some() {
        println!("Sample finished. If it looks right, run `compress` without --sample.");
    }
}
