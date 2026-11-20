// `zazzle flow write docs`: the cheap writer, not the expensive executor, brings the documentation in step with an accepted change: the READMEs and `.dev` records beside and above each changed file (and any paths named), updated only where the change makes them wrong or incomplete; zazzle keeps the writer to documentation paths, checks JSON stays valid and runs the repository's documentation checks, asking once more with the problems and putting everything back if they remain.
use super::edit::{self, FILES};
use super::policy::{Check, Policy};
use super::repo;
use super::verify;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Backend, Log};
use std::path::Path;
use std::time::{Duration, Instant};

// The most documentation files the writer is given, closest to the change first, and the largest one.
const MAX_FILES: usize = 8;
const MAX_BYTES: u64 = 40_000;
// The most tokens of diff the writer sees.
const DIFF_TOKENS: usize = 6000;

// The documentation a change may have made stale: for each changed file that is not documentation, the README.md and `.dev/*.json` records in its folder and each folder above it, closest first, then the documentation the change itself touched and the paths named; at most MAX_FILES, none larger than MAX_BYTES.
pub fn candidates(top: &Path, p: &Policy, changed: &[String], named: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let add = |rel: String, out: &mut Vec<String>| {
        let ok = std::fs::metadata(top.join(&rel)).is_ok_and(|m| m.is_file() && m.len() <= MAX_BYTES);
        if ok && !out.contains(&rel) {
            out.push(rel);
        }
    };
    for path in changed.iter().filter(|c| !p.is_doc(c)) {
        let mut dir = Path::new(path).parent();
        while let Some(d) = dir {
            let rel = |name: &str| d.join(name).to_string_lossy().trim_start_matches('/').to_string();
            add(rel("README.md"), &mut out);
            let mut dev: Vec<String> = std::fs::read_dir(top.join(d).join(".dev"))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".json"))
                .collect();
            dev.sort();
            for n in dev {
                add(rel(&format!(".dev/{n}")), &mut out);
            }
            dir = d.parent();
        }
    }
    for path in changed.iter().filter(|c| p.is_doc(c)).chain(named) {
        add(path.clone(), &mut out);
    }
    out.truncate(MAX_FILES);
    out
}

// The problems with the documentation as it now stands: a changed JSON file that no longer parses, and each documentation check that fails.
fn problems(top: &Path, p: &Policy, changed: &[String], limit: Duration) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for f in changed.iter().filter(|f| f.ends_with(".json")) {
        let text = std::fs::read_to_string(top.join(f)).unwrap_or_default();
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&text) {
            out.push(format!("{f} is not valid JSON: {e}"));
        }
    }
    let logs = repo::state(top, "logs")?;
    for (i, run) in p.docs_checks.iter().enumerate() {
        let c = Check {
            name: format!("docs{}", i + 1),
            run: run.clone(),
            dir: ".".into(),
            kind: "script".into(),
            paths: Vec::new(),
            fix: None,
            report: None,
        };
        let o = verify::run_check(top, &c, &logs.join(format!("docs-check-{}.log", i + 1)), limit);
        if o.status != "passed" {
            out.push(format!("`{run}` {}: {}", o.status, o.failures.join(" / ")));
        }
    }
    Ok(out)
}

// Has the writer update the documentation for the change against `base`, validates it, and returns the report and whether it ended clean.
#[allow(clippy::too_many_arguments)]
pub fn update(
    top: &Path,
    p: &Policy,
    intent: &str,
    base: &str,
    named: &[String],
    b: &dyn Backend,
    writer: &Rung,
    log: &Log,
    limit: Duration,
) -> Result<(String, bool), String> {
    if intent.trim().is_empty() {
        return Err("say what the change is for with --intent".into());
    }
    let before = repo::worktree_tree(top)?;
    let changed = repo::changed(top, base, &before)?;
    let code: Vec<&String> = changed.iter().filter(|c| !p.is_doc(c)).collect();
    if code.is_empty() && named.is_empty() {
        return Ok(("docs: the change touches no code, so no documentation is updated\n".into(), true));
    }
    let docs = candidates(top, p, &changed, named);
    if docs.is_empty() {
        return Ok(("docs: no README or .dev record sits beside or above the changed files; name documentation with --paths\n".into(), true));
    }
    let mut diff = String::new();
    for f in &code {
        diff.push_str(&repo::git(top, &["diff", "-U3", base, &before, "--", f.as_str()]).unwrap_or_default());
        diff.push('\n');
    }
    let diff = cap(&diff, DIFF_TOKENS);
    let current: String = docs
        .iter()
        .map(|d| {
            format!("Current {d}:\n```\n{}```\n", std::fs::read_to_string(top.join(d)).unwrap_or_default())
        })
        .collect();
    let how =
        if writer.hosted() { "Edit the files directly, then reply with the STATUS line." } else { FILES };
    let mut prompt = format!(
        "Update the documentation for this change where the change makes it wrong or incomplete, and nowhere else. Keep each file's structure, style and format (JSON stays valid and keeps its schema and field names), keep it concise, and describe only what the diff shows. Leave files that need nothing unchanged. You may change only: {}.\nIntent: {intent}\nDiff:\n{diff}\n{current}{how}",
        docs.join(", ")
    );
    let allowed = |path: &str| p.is_doc(path) && (docs.iter().any(|d| d == path) || repo::under(path, named));
    let mut out = String::new();
    let mut changed_docs: Vec<String> = Vec::new();
    for round in 0..2 {
        let sections = [("task", intent), ("diff", diff.as_str()), ("docs", current.as_str())];
        let e = edit::edit(
            top,
            b,
            log,
            writer,
            if round == 0 { "docs" } else { "docs retry" },
            &prompt,
            &sections,
            &allowed,
            Instant::now() + limit,
        )?;
        if !e.reverted.is_empty() {
            out.push_str(&format!(
                "the writer changed paths it may not, put back: {}\n",
                e.reverted.join(", ")
            ));
        }
        changed_docs = repo::changed(top, &before, &repo::worktree_tree(top)?)?;
        let left = problems(top, p, &changed_docs, limit)?;
        if left.is_empty() {
            let what = if changed_docs.is_empty() {
                "nothing needed changing".to_string()
            } else {
                format!("updated {}", changed_docs.join(", "))
            };
            out.insert_str(
                0,
                &format!("docs by {}: {what} (looked at {})\n", writer.label(), docs.join(", ")),
            );
            out.push_str("-- documentation done by the writer; review the diff with the change\n");
            return Ok((out, true));
        }
        out.push_str(&format!("round {}: {}\n", round + 1, left.join("; ")));
        prompt.push_str(&format!("\n\nYour update had these problems; fix them:\n- {}", left.join("\n- ")));
    }
    repo::restore(top, &before, &changed_docs)?;
    out.insert_str(0, &format!("docs by {}: not applied\n", writer.label()));
    out.push_str("-- the documentation checks still failed, so the writer's changes were put back\n");
    Ok((out, false))
}

// Cuts text to about `tokens` tokens at a line boundary.
fn cap(text: &str, tokens: usize) -> String {
    let mut out = String::new();
    for l in text.lines() {
        if crate::tokens::estimate(&out) > tokens {
            out.push_str("[diff cut here]\n");
            break;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}
