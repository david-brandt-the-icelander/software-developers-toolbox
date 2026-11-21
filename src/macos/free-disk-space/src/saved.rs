// Measures the disk space transparent compression saves now, for the files compress recorded or under chosen paths.
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde_json::{json, Value};

use crate::decompress::{collect, found, Found};
use crate::scan;
use crate::util::{self, c, human_i};

// Options for the saved command.
pub struct SavedOpts {
    pub state: String,
    pub paths: Vec<String>,
    pub json: bool,
}

// A count of compressed files with the space they would take plain and the space they take now.
#[derive(Default)]
struct Tally {
    files: i64,
    plain: i64,
    on_disk: i64,
}

impl Tally {
    // Count one compressed file.
    fn add(&mut self, f: &Found) {
        self.files += 1;
        self.plain += f.plain;
        self.on_disk += f.alloc;
    }

    // Return the space compression saves: what the files would take plain minus what they take now.
    fn saved(&self) -> i64 {
        self.plain - self.on_disk
    }

    // Return the tally as JSON.
    fn json(&self) -> Value {
        json!({"files": self.files, "plain": self.plain, "on_disk": self.on_disk, "saved": self.saved()})
    }
}

// What the files compress recorded as done look like now.
#[derive(Default)]
struct Recorded {
    still: Tally,
    saved_when_compressed: i64,
    plain_now: i64,
    gone: i64,
}

// Read the files compress recorded as done, in path order, with the saving it verified for each; without a state file
// there are none, and a state file that cannot be opened is an error.
fn recorded(state: &str) -> Result<Vec<(String, i64)>, String> {
    if !Path::new(state).exists() {
        return Ok(Vec::new());
    }
    let fail = |e: rusqlite::Error| format!("cannot open {state}: {e}");
    let db = scan::try_open(state).map_err(fail)?;
    let mut stmt = db
        .prepare("SELECT path, COALESCE(saved,0) FROM cands WHERE status='done' ORDER BY path")
        .map_err(fail)?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(fail)?;
    Ok(rows.flatten().collect())
}

// Return a file's metadata when it is a regular file that still carries the compressed flag.
fn compressed_now(path: &Path) -> Result<Option<fs::Metadata>, ()> {
    let md = fs::symlink_metadata(path).map_err(|_| ())?;
    Ok((md.is_file() && util::flags(&md) & util::UF_COMPRESSED != 0).then_some(md))
}

// Measure the recorded files as they are now: still compressed (each inode once), no longer compressed, or gone.
fn measure_recorded(rows: &[(String, i64)]) -> Recorded {
    let mut r = Recorded::default();
    let mut seen = HashSet::new();
    for (path, saved) in rows {
        r.saved_when_compressed += saved;
        match compressed_now(Path::new(path)) {
            Err(()) => r.gone += 1,
            Ok(None) => r.plain_now += 1,
            Ok(Some(md)) => {
                let f = found(Path::new(path), &md);
                if seen.insert(f.id) {
                    r.still.add(&f);
                }
            }
        }
    }
    r
}

// Return the volume and inode of each recorded file that still exists, so a file counts as compress's under any name.
fn recorded_ids(rows: &[(String, i64)]) -> HashSet<(u64, u64)> {
    rows.iter().filter_map(|(p, _)| fs::symlink_metadata(p).ok()).map(|md| (md.dev(), md.ino())).collect()
}

// Measure every compressed file under the paths (each inode once), and those of them compress recorded; return the
// paths that could not be read.
fn measure_paths(paths: &[String], ours: &HashSet<(u64, u64)>) -> (Tally, Tally, Vec<String>) {
    let (mut all, mut by_us, mut notes, mut seen) =
        (Tally::default(), Tally::default(), Vec::new(), HashSet::new());
    let mut found = Vec::new();
    for p in paths {
        collect(Path::new(&util::expand(p)), None, &mut found, &mut notes);
    }
    for f in found.iter().filter(|f| seen.insert(f.id)) {
        all.add(f);
        if ours.contains(&f.id) {
            by_us.add(f);
        }
    }
    (all, by_us, notes)
}

// Gather what saved reports: for the recorded files, or for every compressed file under the given paths.
fn gather(o: &SavedOpts) -> Value {
    let mut problems = Vec::new();
    let rows = recorded(&o.state).unwrap_or_else(|e| {
        problems.push(e);
        Vec::new()
    });
    let head = json!({
        "tool": "free-disk-space", "version": env!("CARGO_PKG_VERSION"), "ok": problems.is_empty(),
        "problems": problems, "state_file": o.state,
    });
    let mut v = head;
    if o.paths.is_empty() {
        let r = measure_recorded(&rows);
        v["scope"] = json!("recorded");
        v["compressed"] = r.still.json();
        v["recorded"] = json!({
            "files": rows.len(), "saved_when_compressed": r.saved_when_compressed,
            "no_longer_compressed": r.plain_now, "gone": r.gone,
        });
        return v;
    }
    let (all, by_us, notes) = measure_paths(&o.paths, &recorded_ids(&rows));
    let others = Tally {
        files: all.files - by_us.files,
        plain: all.plain - by_us.plain,
        on_disk: all.on_disk - by_us.on_disk,
    };
    v["scope"] = json!("paths");
    v["paths"] = json!(o.paths);
    v["compressed"] = all.json();
    v["by_free_disk_space"] = by_us.json();
    v["by_others"] = others.json();
    v["skipped"] = json!(notes);
    v
}

// Format a tally's line: files, plain and on-disk sizes, and the saving with its share of the plain size.
fn tally_line(t: &Value) -> String {
    let n = |k: &str| t[k].as_i64().unwrap_or(0);
    let pct = if n("plain") > 0 { format!(" ({}%)", n("saved") * 100 / n("plain")) } else { String::new() };
    format!(
        "{} files, {} plain, {} on disk, saved {}{pct}",
        n("files"),
        human_i(n("plain")),
        human_i(n("on_disk")),
        c("1", &human_i(n("saved")))
    )
}

// Render the gathered report as text.
fn text(v: &Value) -> String {
    let n = |x: &Value| x.as_i64().unwrap_or(0);
    let mut out = String::new();
    if v["scope"] == "recorded" {
        let r = &v["recorded"];
        if n(&r["files"]) == 0 {
            out.push_str(
                "No files recorded as compressed. Run compress, or give a PATH to measure what is there.\n",
            );
        } else {
            out.push_str(&format!("Compressed by free-disk-space: {}\n", tally_line(&v["compressed"])));
            out.push_str(&format!(
                "  recorded {} files, saved {} when compressed; {} no longer compressed, {} gone\n",
                n(&r["files"]),
                human_i(n(&r["saved_when_compressed"])),
                n(&r["no_longer_compressed"]),
                n(&r["gone"])
            ));
        }
    } else {
        out.push_str(&format!("Compressed files: {}\n", tally_line(&v["compressed"])));
        out.push_str(&format!("  by free-disk-space: {}\n", tally_line(&v["by_free_disk_space"])));
        out.push_str(&format!("  by others         : {}\n", tally_line(&v["by_others"])));
    }
    for p in v["problems"].as_array().into_iter().flatten() {
        out.push_str(&format!("{} {}\n", c("31", "problem:"), p.as_str().unwrap_or_default()));
    }
    out
}

// Report how much space transparent compression saves now, as text or as one JSON object; read-only.
pub fn cmd_saved(o: &SavedOpts) {
    let v = gather(o);
    if o.json {
        println!("{v}");
        return;
    }
    for note in v["skipped"].as_array().into_iter().flatten() {
        util::warn(&format!("skipped {}", note.as_str().unwrap_or_default()));
    }
    print!("{}", text(&v));
}

// Unit tests for measuring the space compression saves.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair::record;
    use crate::scan::open_db;
    use crate::testkit::Sandbox;

    // Build saved options for the sandbox.
    fn opts(sb: &Sandbox, paths: &[&Path], json: bool) -> SavedOpts {
        SavedOpts {
            state: sb.state(),
            paths: paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            json,
        }
    }

    // Check a tally adds files and reports the saving as plain minus on disk.
    #[test]
    fn test_tally() {
        let mut t = Tally::default();
        t.add(&Found { path: "a".into(), id: (1, 1), alloc: 4096, plain: 16384 });
        t.add(&Found { path: "b".into(), id: (1, 2), alloc: 0, plain: 4096 });
        assert_eq!((t.files, t.plain, t.on_disk, t.saved()), (2, 20480, 4096, 16384));
        assert_eq!(t.json()["saved"], 16384);
    }

    // Check the tally line shows the share saved only when there is a plain size.
    #[test]
    fn test_tally_line() {
        let line = tally_line(&json!({"files": 2, "plain": 8192, "on_disk": 2048, "saved": 6144}));
        assert!(line.starts_with("2 files, 8.0 KiB plain, 2.0 KiB on disk, saved 6.0 KiB (75%)"), "{line}");
        assert!(!tally_line(&json!({"files": 0, "plain": 0, "on_disk": 0, "saved": 0})).contains('%'));
    }

    // Check that without a state file nothing is recorded, and an unreadable one is an error.
    #[test]
    fn test_recorded_missing_and_unreadable() {
        let sb = Sandbox::new();
        assert!(recorded(&sb.state()).unwrap().is_empty());
        let dir = sb.dir("state-is-a-directory");
        assert!(recorded(&dir.to_string_lossy()).unwrap_err().starts_with("cannot open"));
    }

    // Check recorded files are counted as still compressed (a hard link once), no longer compressed, or gone.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_measure_recorded() {
        let sb = Sandbox::new();
        let (a, b) = (sb.write("a", &[7; 10000]), sb.write("b", b"plain"));
        let d = sb.path("d");
        fs::hard_link(&a, &d).unwrap();
        let db = open_db(&sb.state(), true);
        for (p, saved) in [(&a, 6000), (&b, 0), (&d, 0)] {
            record(&db, &p.to_string_lossy(), "done", "", saved);
        }
        record(&db, &sb.path("gone").to_string_lossy(), "done", "", 100);
        record(&db, &sb.path("pending").to_string_lossy(), "pending", "", 0);
        drop(db);
        let rows = recorded(&sb.state()).unwrap();
        assert_eq!(rows.len(), 4);
        // Flags read in path order: a compressed, b plain, d compressed (the same inode as a).
        util::set_flags(&[util::UF_COMPRESSED, 0, util::UF_COMPRESSED]);
        let r = measure_recorded(&rows);
        util::set_flags(&[]);
        assert_eq!((r.still.files, r.still.plain, r.plain_now, r.gone), (1, 12288, 1, 1));
        assert_eq!(r.saved_when_compressed, 6100);
    }

    // Check measuring paths counts every compressed file once and tells free-disk-space's from others'.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_measure_paths() {
        let sb = Sandbox::new();
        let (a, _) = (sb.write("tree/a", b"aaaa"), sb.write("tree/sub/b", b"bbbb"));
        fs::hard_link(&a, sb.path("tree/a-link")).unwrap();
        let ours = recorded_ids(&[(a.to_string_lossy().into_owned(), 0), ("gone".into(), 0)]);
        util::set_flags(&[util::UF_COMPRESSED]);
        let tree = sb.path("tree").to_string_lossy().into_owned();
        let (all, by_us, notes) =
            measure_paths(&[tree, sb.path("nowhere").to_string_lossy().into_owned()], &ours);
        util::set_flags(&[]);
        assert_eq!((all.files, all.plain), (2, 8192));
        assert_eq!(by_us.files, 1, "a and its link are one file, and it is compress's under either name");
        assert_eq!(notes.len(), 1);
    }

    // Check the report in both scopes, as JSON and as text, with and without recorded files.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_gather_and_text() {
        let sb = Sandbox::new();
        let v = gather(&opts(&sb, &[], false));
        assert_eq!((v["ok"].clone(), v["scope"].clone()), (json!(true), json!("recorded")));
        assert!(text(&v).starts_with("No files recorded as compressed"));
        let a = sb.write("tree/a", &[1; 9000]);
        sb.write("tree/b", &[2; 9000]);
        let db = open_db(&sb.state(), true);
        record(&db, &a.to_string_lossy(), "done", "", 4000);
        drop(db);
        util::set_flags(&[util::UF_COMPRESSED]);
        let v = gather(&opts(&sb, &[], false));
        assert_eq!(v["compressed"]["files"], 1);
        assert_eq!(v["recorded"]["saved_when_compressed"], 4000);
        assert!(text(&v).contains("Compressed by free-disk-space: 1 files"));
        let v = gather(&opts(&sb, &[&sb.path("tree")], false));
        util::set_flags(&[]);
        assert_eq!(
            (v["compressed"]["files"].clone(), v["by_free_disk_space"]["files"].clone()),
            (json!(2), json!(1))
        );
        assert_eq!(v["by_others"]["files"], 1);
        assert!(text(&v).contains("by others"));
        let broken =
            SavedOpts { state: sb.dir("bad").to_string_lossy().into_owned(), paths: vec![], json: false };
        let v = gather(&broken);
        assert_eq!(v["ok"], false);
        assert!(text(&v).contains("problem:"));
    }

    // Check the command prints text with warnings, and JSON.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_cmd_saved() {
        let sb = Sandbox::new();
        sb.write("tree/a", b"data");
        util::set_flags(&[util::UF_COMPRESSED]);
        cmd_saved(&opts(&sb, &[&sb.path("tree"), &sb.path("nowhere")], false));
        cmd_saved(&opts(&sb, &[&sb.path("tree")], true));
        cmd_saved(&opts(&sb, &[], false));
        util::set_flags(&[]);
    }

    // Check on macOS that a file compressed by the library and recorded as done is measured as saving space.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_saved_macos() {
        let sb = Sandbox::new();
        let text = b"free-disk-space measures what this text saves when compressed. ".repeat(4000);
        let src = sb.write("data/src", &text);
        let file = sb.write("data/f", &text);
        let target = fs::OpenOptions::new().read(true).write(true).open(&file).unwrap();
        crate::lzfse::compress_onto(&fs::File::open(&src).unwrap(), &target).unwrap();
        drop(target);
        let db = open_db(&sb.state(), true);
        record(&db, &file.to_string_lossy(), "done", "", 1);
        drop(db);
        let v = gather(&opts(&sb, &[], false));
        assert_eq!(v["compressed"]["files"], 1);
        assert!(v["compressed"]["saved"].as_i64().unwrap() > 0);
        let v = gather(&opts(&sb, &[&sb.path("data")], true));
        assert_eq!(v["by_free_disk_space"]["files"], 1);
        assert_eq!(v["by_others"]["files"], 0);
    }
}
