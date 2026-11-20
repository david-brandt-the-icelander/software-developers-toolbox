// `zazzle flow write commit|changelog|pr`: a cheap writer drafts the prose from the facts the orchestrator settled (the diff, the intent, the compatibility decision) and the verification receipt, in the repository's own format; zazzle validates the draft (subject length, the template's headings, changelog sections, no invented facts) and asks once more with the problems before giving up.
use super::policy::Policy;
use super::repo;
use super::verify;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Ask, Backend, Log};
use std::path::Path;
use std::time::Instant;

// The most tokens of diff a writer sees.
const DIFF_TOKENS: usize = 6000;
// Changelog section names when the file has none of its own.
const SECTIONS: &[&str] = &["Added", "Changed", "Deprecated", "Removed", "Fixed", "Security"];

// What the writer drafts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Commit,
    Changelog,
    Pr,
}

// The facts a writer gets: the kind, the intent, the compatibility decision, and the base to diff against.
pub struct Facts<'a> {
    pub kind: Kind,
    pub intent: &'a str,
    pub compat: Option<&'a str>,
    pub base: &'a str,
}

// The diff the writer describes, cut to DIFF_TOKENS: the staged change for a commit (else the working tree against HEAD), the working tree against the base otherwise.
fn the_diff(top: &Path, f: &Facts) -> Result<String, String> {
    let d = match f.kind {
        Kind::Commit => {
            let staged = repo::git(top, &["diff", "--cached", "-U3"])?;
            if staged.is_empty() {
                repo::diff(top, "HEAD", &repo::worktree_tree(top)?)?
            } else {
                staged
            }
        }
        _ => repo::diff(top, f.base, &repo::worktree_tree(top)?)?,
    };
    let mut out = String::new();
    for l in d.lines() {
        if crate::tokens::estimate(&out) > DIFF_TOKENS {
            out.push_str("[diff cut here]\n");
            break;
        }
        out.push_str(l);
        out.push('\n');
    }
    Ok(out)
}

// The headings of a markdown template, as written.
pub fn headings(template: &str) -> Vec<String> {
    template.lines().filter(|l| l.starts_with('#')).map(|l| l.trim().to_string()).collect()
}

// The changelog's own section names (`### Added` ...), else the usual ones.
fn sections(changelog: &str) -> Vec<String> {
    let own: Vec<String> = changelog
        .lines()
        .filter_map(|l| l.strip_prefix("### "))
        .map(|s| s.trim().to_string())
        .fold(Vec::new(), |mut v, s| {
            if !v.contains(&s) {
                v.push(s)
            }
            v
        });
    if own.is_empty() {
        SECTIONS.iter().map(|s| s.to_string()).collect()
    } else {
        own
    }
}

// The problems with a draft: a missing fact it admits, a long or empty subject, a template heading left out, or a changelog line outside a known section.
pub fn problems(kind: Kind, draft: &str, template: Option<&str>, changelog: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(l) = draft.lines().find(|l| l.contains("MISSING:")) {
        out.push(format!("it says a fact is missing: {}", l.trim()));
    }
    match kind {
        Kind::Commit => {
            let mut lines = draft.lines();
            let subject = lines.next().unwrap_or_default();
            if subject.trim().is_empty() {
                out.push("the subject line is empty".into());
            } else if subject.chars().count() > 72 {
                out.push(format!("the subject has {} characters; keep it to 72", subject.chars().count()));
            }
            if lines.next().is_some_and(|l| !l.trim().is_empty()) {
                out.push("the subject needs a blank line after it".into());
            }
        }
        Kind::Pr => {
            for h in headings(template.unwrap_or_default()) {
                if !draft.lines().any(|l| l.trim() == h) {
                    out.push(format!("the template heading `{h}` is missing"));
                }
            }
        }
        Kind::Changelog => {
            let known = sections(changelog.unwrap_or_default());
            let mut in_section = false;
            for l in draft.lines().filter(|l| !l.trim().is_empty()) {
                if let Some(s) = l.strip_prefix("### ") {
                    in_section = known.iter().any(|k| k == s.trim());
                    if !in_section {
                        out.push(format!(
                            "`### {}` is not one of the changelog's sections ({})",
                            s.trim(),
                            known.join(", ")
                        ));
                    }
                } else if !in_section {
                    out.push(format!("`{}` is not under a `### Section` heading", l.trim()));
                }
            }
        }
    }
    out
}

// Inserts a changelog entry under `## [Unreleased]`, adding that heading after the title when the file has none.
pub fn insert(changelog: &str, entry: &str) -> String {
    let lines: Vec<&str> = changelog.lines().collect();
    let entry = entry.trim_end();
    if let Some(i) = lines.iter().position(|l| {
        l.trim().eq_ignore_ascii_case("## [unreleased]") || l.trim().eq_ignore_ascii_case("## unreleased")
    }) {
        let mut out: Vec<String> = lines[..=i].iter().map(|s| s.to_string()).collect();
        out.push(String::new());
        out.push(entry.to_string());
        let rest: Vec<String> =
            lines[i + 1..].iter().skip_while(|l| l.trim().is_empty()).map(|s| s.to_string()).collect();
        if !rest.is_empty() {
            out.push(String::new());
        }
        out.extend(rest);
        return out.join("\n") + "\n";
    }
    let title = lines.iter().position(|l| l.starts_with("# ")).map_or(0, |i| i + 1);
    let mut out: Vec<String> = lines[..title].iter().map(|s| s.to_string()).collect();
    if title > 0 {
        out.push(String::new());
    }
    out.extend(["## [Unreleased]".to_string(), String::new(), entry.to_string()]);
    let rest: Vec<String> =
        lines[title..].iter().skip_while(|l| l.trim().is_empty()).map(|s| s.to_string()).collect();
    if !rest.is_empty() {
        out.push(String::new());
    }
    out.extend(rest);
    out.join("\n") + "\n"
}

// Drafts the prose with the writer and validates it, asking once more with the problems; returns the draft and any problems left.
pub fn write(
    top: &Path,
    p: &Policy,
    f: &Facts,
    b: &dyn Backend,
    writer: &Rung,
    log: &Log,
    deadline: Instant,
) -> Result<(String, Vec<String>), String> {
    if f.kind != Kind::Commit && f.compat.is_none() {
        return Err("the compatibility decision is yours to make: pass --compat (for example 'none: no client sees a change', or what clients must do)".into());
    }
    if f.intent.trim().is_empty() {
        return Err("say what the change is for with --intent".into());
    }
    let diff = the_diff(top, f)?;
    if diff.trim().is_empty() {
        return Err("there is no change to describe".into());
    }
    let tree = repo::worktree_tree(top)?;
    let receipt = verify::receipt_for(top, &tree).map_or_else(
        || format!("no verification of tree {} yet: say so; do not claim tests ran", verify::short(&tree)),
        |r| r.text(),
    );
    let template = p.pr_template.as_ref().and_then(|t| std::fs::read_to_string(top.join(t)).ok());
    let changelog = p.changelog.as_ref().and_then(|c| std::fs::read_to_string(top.join(c)).ok());
    let shape = match f.kind {
        Kind::Commit => "a git commit message: a subject of at most 72 characters in the imperative, a blank line, then a few short lines on what changed and why. No headings.".to_string(),
        Kind::Changelog => format!("a changelog entry: one or more `### Section` headings from ({}) with `- ` lines under them, written for the people who use the software. Nothing else.", sections(changelog.as_deref().unwrap_or_default()).join(", ")),
        Kind::Pr => match &template {
            Some(t) => format!("a pull request description that keeps every heading of this template, in order, filling each (write `None` where nothing applies; skip nothing):\n{t}"),
            None => "a pull request description: what changes, why, compatibility, and how it was verified, in a few short sections.".into(),
        },
    };
    let system = format!("You write {shape}\nUse only the facts given: the diff, the intent, the compatibility decision and the verification receipt. Never invent behavior, compatibility or test results. If a fact the text needs is not given, write `MISSING: what` instead. Be concise.");
    let compat = f.compat.unwrap_or("not needed for a commit message");
    let mut prompt = format!(
        "Intent: {}\nCompatibility decision: {compat}\nVerification:\n{receipt}\nDiff:\n{diff}",
        f.intent
    );
    let sections = [
        ("brief", system.as_str()),
        ("task", f.intent),
        ("evidence", receipt.as_str()),
        ("diff", diff.as_str()),
    ];
    let ask = |role: &str, prompt: &str| {
        log.text(b, writer, &Ask::new(role, &system, prompt, &sections), top, deadline)
            .map(|d| strip_fence(&d))
    };
    let mut draft = ask("write", &prompt)?;
    let mut left = problems(f.kind, &draft, template.as_deref(), changelog.as_deref());
    if !left.is_empty() {
        prompt.push_str(&format!("\n\nYour draft had these problems; fix them and reply with the whole text again:\n- {}\nYour draft:\n{draft}", left.join("\n- ")));
        draft = ask("write retry", &prompt)?;
        left = problems(f.kind, &draft, template.as_deref(), changelog.as_deref());
    }
    Ok((draft, left))
}

// Drops a markdown fence the writer wrapped its whole answer in.
fn strip_fence(s: &str) -> String {
    let t = s.trim();
    match t.strip_prefix("```") {
        Some(rest) if t.ends_with("```") && rest.len() >= 3 => {
            let body = &rest[..rest.len() - 3];
            body.split_once('\n').map_or(body, |(_, b)| b).trim().to_string()
        }
        _ => t.to_string(),
    }
}
