// Shows scan progress, candidates, free space and the daemon, as text or as one JSON object for scripts.
use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::daemon;
use crate::scan::{self, Stats};
use crate::util::{self, c, die, hms, human_i};

// Read scan progress from an open state file.
fn scan_json(db: &Connection) -> Value {
    let cfg = scan::meta_get(db, "cfg").unwrap_or(Value::Null);
    let s = scan::meta_get(db, "stats").map(|v| Stats::from_json(&v)).unwrap_or_default();
    let elapsed = scan::meta_get(db, "elapsed").and_then(|v| v.as_f64()).unwrap_or(0.0);
    json!({
        "root": cfg["root"], "complete": scan::finished(db), "dirs_queued": scan::pending_dirs(db),
        "dirs": s.dirs, "files": s.files, "bytes": s.bytes, "errors": s.errors, "elapsed_secs": elapsed,
    })
}

// Count candidates by status, with their estimated and verified savings.
fn candidates_json(db: &Connection) -> Value {
    let sql = "SELECT status, COUNT(*), SUM(est_saved), SUM(saved) FROM cands GROUP BY status";
    let mut out = Map::new();
    let Ok(mut stmt) = db.prepare(sql) else { return Value::Object(out) };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    });
    for (status, n, est, saved) in rows.into_iter().flatten().flatten() {
        out.insert(status, json!({"files": n, "estimated": est.unwrap_or(0), "saved": saved.unwrap_or(0)}));
    }
    Value::Object(out)
}

// Count the file names the scan recorded and those replaced by clones.
fn names_json(db: &Connection) -> Value {
    let (names, cloned): (i64, Option<i64>) = db
        .query_row("SELECT COUNT(*), SUM(cloned=1) FROM files", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((0, None));
    json!({"recorded": names, "cloned": cloned.unwrap_or(0)})
}

// Gather everything status shows; "ok" is false, with the reason in "problems", when the state file cannot be read,
// files are recorded as broken, free space cannot be read or the installed daemon is not running.
pub fn gather(state: &str) -> Value {
    let mut problems: Vec<String> = Vec::new();
    let (mut scan, mut cands, mut names) = (Value::Null, json!({}), Value::Null);
    if Path::new(state).exists() {
        match scan::try_open(state) {
            Ok(db) => (scan, cands, names) = (scan_json(&db), candidates_json(&db), names_json(&db)),
            Err(e) => problems.push(format!("cannot open {state}: {e}")),
        }
    }
    if let Some(n) = cands["broken"]["files"].as_i64() {
        problems.push(format!("{n} files recorded as broken (list --status broken)"));
    }
    let space = util::data_volume_space();
    let free = space.map_or(
        Value::Null,
        |(total, free)| json!({"volume": util::data_volume(), "total": total, "free": free}),
    );
    if space.is_none() {
        problems.push("cannot read free space".into());
    }
    let (installed, running) = daemon::state();
    if installed && !running {
        problems.push("daemon installed but not running".into());
    }
    json!({
        "tool": "free-disk-space", "version": env!("CARGO_PKG_VERSION"), "ok": problems.is_empty(),
        "problems": problems, "state_file": state, "scan": scan, "candidates": cands, "file_names": names,
        "free_space": free,
        "daemon": {"installed": installed, "running": running, "plist": daemon::plist_path(), "log": daemon::log_path()},
    })
}

// Render the gathered status as the text status has always printed, plus the daemon and any problems.
fn text(v: &Value) -> String {
    let s = &v["scan"];
    let mut out = format!("{}: root {}\n", c("1", "Scan"), s["root"].as_str().unwrap_or("None"));
    let state = if s["complete"] == true {
        "complete".to_string()
    } else {
        format!("in progress ({} dirs queued)", s["dirs_queued"])
    };
    out.push_str(&format!("  state     : {state}\n"));
    let n = |x: &Value| x.as_i64().unwrap_or(0);
    out.push_str(&format!(
        "  processed : {} dirs, {} files, {}, {} errors, {} elapsed\n",
        s["dirs"],
        s["files"],
        human_i(n(&s["bytes"])),
        s["errors"],
        hms(s["elapsed_secs"].as_f64().unwrap_or(0.0))
    ));
    out.push_str(&format!("{}\n", c("1", "Candidates")));
    for (status, row) in v["candidates"].as_object().into_iter().flatten() {
        out.push_str(&format!(
            "  {status:<8} {:>7} files   estimated {:<10} verified saved {}\n",
            n(&row["files"]),
            human_i(n(&row["estimated"])),
            human_i(n(&row["saved"]))
        ));
    }
    let names = &v["file_names"];
    out.push_str(&format!(
        "{}: {} recorded, {} replaced by clones\n",
        c("1", "File names"),
        names["recorded"],
        names["cloned"]
    ));
    let free = &v["free_space"];
    if let Some(volume) = free["volume"].as_str() {
        out.push_str(&format!("Current free space on {volume}: {}\n", c("1", &human_i(n(&free["free"])))));
    }
    let d = &v["daemon"];
    let installed = if d["installed"] == true { "installed" } else { "not installed" };
    let running = if d["running"] == true { "running" } else { "not running" };
    out.push_str(&format!("{}: {installed}, {running}\n", c("1", "Daemon")));
    for p in v["problems"].as_array().into_iter().flatten() {
        out.push_str(&format!("{} {}\n", c("31", "problem:"), p.as_str().unwrap_or_default()));
    }
    out
}

// Show scan progress, candidates, free space and the daemon; text needs a scan state file, JSON works without one.
pub fn cmd_status(state: &str, as_json: bool) {
    let v = gather(state);
    if as_json {
        println!("{v}");
        return;
    }
    if v["scan"].is_null() {
        let first = v["problems"][0].as_str().filter(|p| p.starts_with("cannot open"));
        die(first.unwrap_or(&format!("no scan state at {state} - start one with: free-disk-space scan")));
    }
    print!("{}", text(&v));
}

// Unit tests for the status command.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sandbox;

    // Check JSON status without a state file: ok, no scan, free space and the daemon still reported.
    #[test]
    fn test_gather_without_state() {
        let sb = Sandbox::new();
        let v = gather(&sb.state());
        assert_eq!((v["tool"].as_str(), v["ok"].as_bool()), (Some("free-disk-space"), Some(true)));
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert!(v["scan"].is_null());
        assert!(v["free_space"]["free"].as_i64().unwrap() > 0);
        assert_eq!(v["daemon"]["installed"], false);
        cmd_status(&sb.state(), true);
        assert!(std::panic::catch_unwind(|| cmd_status(&sb.state(), false)).is_err());
    }

    // Check a populated state file: progress, candidates by status and names, with broken files making it not ok.
    #[test]
    fn test_gather_with_state() {
        let sb = Sandbox::new();
        let db = scan::open_db(&sb.state(), true);
        scan::meta_set(&db, "cfg", &json!({"root": "/r"}));
        scan::meta_set(&db, "stats", &json!({"dirs": 3, "files": 9, "bytes": 2048, "errors": 1}));
        db.execute_batch(
            "INSERT INTO cands(path,size,est_saved,status,saved) VALUES ('/a',100,50,'pending',0),('/b',9,5,'done',7);
             INSERT INTO files(path,name,size,cloned) VALUES ('/a','a',1,1),('/b','b',1,0);
             INSERT INTO dirs(path) VALUES ('/x');",
        )
        .unwrap();
        let v = gather(&sb.state());
        assert_eq!(v["ok"], true);
        assert_eq!(v["scan"]["root"], "/r");
        assert_eq!(
            (v["scan"]["complete"].as_bool(), v["scan"]["dirs_queued"].as_i64()),
            (Some(false), Some(1))
        );
        assert_eq!(v["candidates"]["done"]["saved"], 7);
        assert_eq!(
            (v["file_names"]["recorded"].as_i64(), v["file_names"]["cloned"].as_i64()),
            (Some(2), Some(1))
        );
        let out = text(&v);
        assert!(out.contains("in progress (1 dirs queued)"));
        assert!(out.contains("Daemon: not installed, not running"));
        assert!(out.contains("  done           1 files"));
        db.execute("INSERT INTO cands(path,size,est_saved,status) VALUES ('/c',1,0,'broken')", []).unwrap();
        scan::meta_set(&db, "finished", &json!(1.0));
        let v = gather(&sb.state());
        assert_eq!(v["ok"], false);
        assert!(v["problems"][0].as_str().unwrap().starts_with("1 files recorded as broken"));
        let out = text(&v);
        assert!(out.contains("state     : complete"));
        assert!(out.contains("problem: 1 files recorded as broken"));
        cmd_status(&sb.state(), false);
    }

    // Check an unreadable state file and an installed daemon that is not running are reported as problems.
    #[test]
    fn test_gather_problems() {
        let sb = Sandbox::new();
        std::fs::write(sb.state(), b"this is not a database, just text that is long enough to be read")
            .unwrap();
        let plist = daemon::plist_path();
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"<plist/>").unwrap();
        let v = gather(&sb.state());
        assert_eq!(v["ok"], false);
        let problems = v["problems"].to_string();
        assert!(problems.contains("cannot open"), "{problems}");
        assert!(problems.contains("daemon installed but not running"), "{problems}");
        let state = sb.state();
        let err = std::panic::catch_unwind(|| cmd_status(&state, false)).unwrap_err();
        assert!(err.downcast_ref::<String>().unwrap().contains("cannot open"));
        sb.tool("launchctl", "exit 0");
        assert_eq!(gather(&sb.state())["daemon"]["running"], true);
        assert!(text(&json!({})).contains("Scan: root None"));
        let up = json!({"daemon": {"installed": true, "running": true}});
        assert!(text(&up).contains("Daemon: installed, running"));
    }
}
