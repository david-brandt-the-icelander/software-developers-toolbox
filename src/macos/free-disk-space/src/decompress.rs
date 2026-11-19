// Reverses compress for chosen files and folders: rewrites each transparently compressed file plain, verified.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::compress::{self, CompressOpts};
use crate::repair::{plain_copy, record};
use crate::scan::open_db;
use crate::util::{self, c, die, human_i};

// Options for the decompress command.
pub struct DecompressOpts {
    pub state: String,
    pub paths: Vec<String>,
    pub apply: bool,
    pub reserve: i64,
    pub include_no_provenance: bool,
}

// The note recorded in the state file for a decompressed file, so compress and the daemon leave it alone.
const NOTE: &str = "decompressed on request";

// A compressed file under the given paths, with the disk space it takes now and would take plain.
struct Found {
    path: PathBuf,
    alloc: i64,
    plain: i64,
}

// Return the disk space a plain file of this length takes, in whole 4 KiB allocation blocks.
fn plain_alloc(len: u64) -> i64 {
    (len.div_ceil(4096) * 4096) as i64
}

// Collect the compressed regular files under a path, without following symlinks or entering another volume than
// `dev`; note what cannot be read.
fn collect(path: &Path, dev: Option<u64>, out: &mut Vec<Found>, notes: &mut Vec<String>) {
    let md = match fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(e) => return notes.push(format!("{}: {e}", path.display())),
    };
    if dev.is_some_and(|d| d != md.dev()) {
        return notes.push(format!("{}: another volume, not entered", path.display()));
    }
    if md.is_dir() {
        match fs::read_dir(path) {
            Ok(entries) => entries.flatten().for_each(|e| collect(&e.path(), Some(md.dev()), out, notes)),
            Err(e) => notes.push(format!("{}: {e}", path.display())),
        }
        return;
    }
    if md.is_file() && util::flags(&md) & util::UF_COMPRESSED != 0 {
        out.push(Found { path: path.to_path_buf(), alloc: util::alloc(&md), plain: plain_alloc(md.len()) });
    }
}

// Build the compress options whose safety checks decompress shares: any file age, and the given reserve.
fn compress_opts(o: &DecompressOpts) -> CompressOpts {
    CompressOpts {
        state: o.state.clone(),
        sample: None,
        max_files: None,
        limit: None,
        min_age_days: 0,
        min_gain: 0,
        reserve: o.reserve,
        dry_run: !o.apply,
        yes: true,
        include_no_provenance: o.include_no_provenance,
        retry_failed: false,
    }
}

// List each file with the space it would add, or why it would be skipped.
fn preview(found: &[Found], co: &CompressOpts) {
    for f in found {
        let note = match fs::symlink_metadata(&f.path) {
            Ok(st) => compress::decompress_precheck(&f.path, &st, co),
            Err(e) => Some(format!("gone: {e}")),
        };
        match note {
            None => println!("  {:>10}  {}", format!("+{}", human_i(f.plain - f.alloc)), f.path.display()),
            Some(note) => println!("  {:>10}  {}: {note}", "skip", f.path.display()),
        }
    }
}

// Rewrite each file plain through a verified copy, record it in the state file and report the change.
fn apply(found: &[Found], co: &CompressOpts) {
    util::install_sigint();
    let db = Path::new(&co.state).exists().then(|| open_db(&co.state, false));
    let free0 = util::report_free("Initial");
    let (mut done, mut skipped, mut failed, mut grown) = (0, 0, 0, 0i64);
    for f in found {
        if util::stopped() {
            break;
        }
        let path = f.path.to_string_lossy();
        let (status, note, saved) = compress::decompress_flagged(&f.path, co, &plain_copy);
        match status {
            "done" => {
                done += 1;
                grown -= saved;
                if let Some(db) = &db {
                    record(db, &path, "skipped", NOTE, 0);
                }
                println!("decompressed {path} (+{})", human_i(-saved));
            }
            "skipped" => {
                skipped += 1;
                println!("skipped {path}: {note}");
            }
            _ => {
                failed += 1;
                println!("{} {path}: {note}; left as it was", c("31", "FAILED"));
            }
        }
    }
    let free1 = util::report_free("Final");
    println!(
        "Decompressed {done}, skipped {skipped}, failed {failed}; they now take {} more",
        human_i(grown)
    );
    println!("Overall free-space change on the volume: {}", c("1", &human_i(free1 - free0)));
}

// Find the compressed files under the paths, report the space decompressing takes and, with --apply, do it.
pub fn cmd_decompress(o: &DecompressOpts) {
    if !cfg!(target_os = "macos") && std::env::var_os("FDS_FORCE").is_none() {
        die("decompress requires macOS (APFS transparent compression)");
    }
    let (mut found, mut notes) = (Vec::new(), Vec::new());
    for p in &o.paths {
        collect(Path::new(&util::expand(p)), None, &mut found, &mut notes);
    }
    for note in &notes {
        util::warn(&format!("skipped {note}"));
    }
    if found.is_empty() {
        println!("No compressed files found.");
        return;
    }
    let (now, plain) =
        (found.iter().map(|f| f.alloc).sum::<i64>(), found.iter().map(|f| f.plain).sum::<i64>());
    let need = plain - now;
    println!(
        "{} compressed files: {} on disk, {} plain (+{})",
        found.len(),
        human_i(now),
        human_i(plain),
        human_i(need)
    );
    let free = util::free_bytes();
    let short = free < need + o.reserve;
    let space =
        format!("needs {} plus {} reserve, {} free", human_i(need), human_i(o.reserve), human_i(free));
    let co = compress_opts(o);
    if o.apply && short {
        die(&format!("not enough free space to decompress: {space}; nothing was changed"));
    }
    if o.apply {
        return apply(&found, &co);
    }
    preview(&found, &co);
    if short {
        util::warn(&format!("not enough free space to decompress: {space}"));
    }
    println!("Dry run - nothing changed ({space}). Run with --apply to decompress.");
}

// Unit tests for decompressing files.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{chmod, is_root, Sandbox};

    // Build decompress options for the sandbox with no reserve, accepting files without a provenance.
    fn opts(sb: &Sandbox, paths: &[&Path], apply: bool) -> DecompressOpts {
        DecompressOpts {
            state: sb.state(),
            paths: paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            apply,
            reserve: 0,
            include_no_provenance: true,
        }
    }

    // Read a file's recorded status and note.
    fn status_of(sb: &Sandbox, path: &Path) -> (String, String) {
        let db = open_db(&sb.state(), false);
        let sql = "SELECT status, COALESCE(note,'') FROM cands WHERE path=?";
        db.query_row(sql, [path.to_string_lossy()], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    }

    // Check a plain file's size on disk rounds up to whole blocks.
    #[test]
    fn test_plain_alloc() {
        assert_eq!(
            (plain_alloc(0), plain_alloc(1), plain_alloc(4096), plain_alloc(4097)),
            (0, 4096, 4096, 8192)
        );
    }

    // Check collection keeps compressed files only, skips symlinks, notes missing, unreadable and foreign paths.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_collect() {
        let sb = Sandbox::new();
        let file = sb.write("d/sub/f", b"data");
        std::os::unix::fs::symlink(&file, sb.path("d/link")).unwrap();
        let (mut found, mut notes) = (Vec::new(), Vec::new());
        util::set_flags(&[0]);
        collect(&sb.path("d"), None, &mut found, &mut notes);
        assert!(found.is_empty());
        assert!(notes.is_empty());
        util::set_flags(&[util::UF_COMPRESSED]);
        collect(&sb.path("d"), None, &mut found, &mut notes);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].path.clone(), found[0].plain), (file, 4096));
        collect(&sb.path("missing"), None, &mut found, &mut notes);
        collect(&sb.path("d"), Some(u64::MAX), &mut found, &mut notes);
        assert!(notes[0].contains("missing"));
        assert!(notes[1].ends_with("another volume, not entered"));
        chmod(&sb.path("d/sub"), 0o000);
        collect(&sb.path("d"), None, &mut found, &mut notes);
        chmod(&sb.path("d/sub"), 0o755);
        if !is_root() {
            assert_eq!(notes.len(), 3);
        }
        util::set_flags(&[]);
    }

    // Check the dry run changes nothing, reports skips and short space, and --apply rewrites, records and refuses.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_cmd_decompress_linux() {
        let sb = Sandbox::new();
        std::env::set_var("FDS_FORCE", "1");
        sb.tool("lsof", "exit 1");
        let text = b"compressed text ".repeat(1000);
        let file = sb.write("data/f", &text);
        drop(open_db(&sb.state(), true));
        util::set_flags(&[0]);
        cmd_decompress(&opts(&sb, &[&sb.path("data")], false));
        util::set_flags(&[util::UF_COMPRESSED]);
        cmd_decompress(&opts(&sb, &[&sb.path("data"), &sb.path("nowhere")], false));
        let strict = DecompressOpts {
            include_no_provenance: false,
            reserve: i64::MAX / 4,
            ..opts(&sb, &[&file], false)
        };
        cmd_decompress(&strict);
        let greedy = DecompressOpts { reserve: i64::MAX / 4, ..opts(&sb, &[&file], true) };
        assert!(std::panic::catch_unwind(|| cmd_decompress(&greedy)).is_err());
        // Flags read: collect, the precheck and the copy see the original as compressed, the copy reads plain, the original again.
        util::set_flags(&[
            util::UF_COMPRESSED,
            util::UF_COMPRESSED,
            util::UF_COMPRESSED,
            0,
            util::UF_COMPRESSED,
        ]);
        cmd_decompress(&opts(&sb, &[&file], true));
        assert_eq!(fs::read(&file).unwrap(), text);
        assert_eq!(status_of(&sb, &file), ("skipped".to_string(), NOTE.to_string()));
        util::set_flags(&[util::UF_COMPRESSED]);
        cmd_decompress(&DecompressOpts { include_no_provenance: false, ..opts(&sb, &[&file], true) });
        util::set_flags(&[util::UF_COMPRESSED, util::UF_COMPRESSED, 0x8000 | util::UF_COMPRESSED]);
        cmd_decompress(&opts(&sb, &[&file], true));
        assert_eq!(fs::read(&file).unwrap(), text);
        std::fs::remove_file(sb.state()).unwrap();
        util::set_flags(&[
            util::UF_COMPRESSED,
            util::UF_COMPRESSED,
            util::UF_COMPRESSED,
            0,
            util::UF_COMPRESSED,
        ]);
        cmd_decompress(&opts(&sb, &[&file], true));
        assert!(!std::path::Path::new(&sb.state()).exists(), "no state file is created");
        util::set_stop(true);
        cmd_decompress(&opts(&sb, &[&file], true));
        util::set_flags(&[]);
    }

    // Check found files that vanish before the preview are reported as gone.
    #[test]
    fn test_preview_gone() {
        let sb = Sandbox::new();
        let gone = Found { path: sb.path("gone"), alloc: 0, plain: 4096 };
        preview(&[gone], &compress_opts(&opts(&sb, &[], false)));
    }

    // Check the command refuses to run off macOS unless forced, and says when nothing is compressed.
    #[test]
    fn test_cmd_decompress_guards() {
        let sb = Sandbox::new();
        let o = opts(&sb, &[&sb.dir("empty")], false);
        if !cfg!(target_os = "macos") {
            assert!(std::panic::catch_unwind(|| cmd_decompress(&o)).is_err());
            std::env::set_var("FDS_FORCE", "1");
        }
        cmd_decompress(&o);
    }

    // Check on macOS that a file compressed by the library is listed by the dry run and rewritten plain by --apply.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_cmd_decompress_macos() {
        let sb = Sandbox::new();
        sb.tool("lsof", "exit 1");
        let text = b"free-disk-space compresses and then decompresses this text. ".repeat(4000);
        let src = sb.write("data/src", &text);
        let file = sb.write("data/f", &text);
        let target = fs::OpenOptions::new().read(true).write(true).open(&file).unwrap();
        crate::lzfse::compress_onto(&fs::File::open(&src).unwrap(), &target).unwrap();
        drop(target);
        assert_ne!(util::flags(&fs::symlink_metadata(&file).unwrap()) & util::UF_COMPRESSED, 0);
        let (mut found, mut notes) = (Vec::new(), Vec::new());
        collect(&sb.path("data"), None, &mut found, &mut notes);
        assert_eq!(found.len(), 1);
        assert!(found[0].plain > found[0].alloc);
        cmd_decompress(&opts(&sb, &[&sb.path("data")], false));
        cmd_decompress(&opts(&sb, &[&file], true));
        assert_eq!(util::flags(&fs::symlink_metadata(&file).unwrap()) & util::UF_COMPRESSED, 0);
        assert_eq!(fs::read(&file).unwrap(), text);
    }
}
