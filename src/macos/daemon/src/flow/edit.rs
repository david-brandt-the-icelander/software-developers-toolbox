// How a delegated agent changes files: a hosted agent edits the working tree itself (allowed to write, never to run commands); a local model replies with whole files (`FILE path` then a fenced block) that zazzle writes; either way every change outside the paths it was given is put back, and the paths it changed are returned.
use super::repo;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Ask, Backend, Log};
use std::path::Path;
use std::time::Instant;

// How a local model hands back files.
pub const FILES: &str = "Reply with each file you change as a line `FILE path` followed by the whole new file in a ``` block, then the STATUS and CONFIDENCE lines. Nothing else.";

// What an edit did: the paths changed inside its scope, those put back for being outside it, and the agent's reply.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Edited {
    pub changed: Vec<String>,
    pub reverted: Vec<String>,
    pub reply: String,
}

// Reads `FILE path` lines each followed by a fenced block into (path, content).
pub fn parse_files(reply: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = reply.lines().peekable();
    while let Some(l) = lines.next() {
        let Some(path) = crate::investigate::cite::strip_word(l.trim(), "FILE") else { continue };
        let path = path.trim_matches('`').to_string();
        while lines.peek().is_some_and(|n| n.trim().is_empty()) {
            lines.next();
        }
        if !lines.peek().is_some_and(|n| n.trim_start().starts_with("```")) {
            continue;
        }
        lines.next();
        let mut body = String::new();
        for b in lines.by_ref() {
            if b.trim_start().starts_with("```") {
                break;
            }
            body.push_str(b);
            body.push('\n');
        }
        if !path.is_empty() {
            out.push((path, body));
        }
    }
    out
}

// Has the agent edit the working tree for `prompt`, keeping only changes to paths `allowed` accepts.
#[allow(clippy::too_many_arguments)]
pub fn edit(
    top: &Path,
    b: &dyn Backend,
    log: &Log,
    r: &Rung,
    role: &str,
    prompt: &str,
    sections: &[(&str, &str)],
    allowed: &dyn Fn(&str) -> bool,
    deadline: Instant,
) -> Result<Edited, String> {
    let before = repo::worktree_tree(top)?;
    let mut e = Edited::default();
    match r.hosted() {
        true => {
            let ask = Ask { role, system: "", prompt, sections, write: true, max_tokens: 0 };
            e.reply = log.hosted(b, r, &ask, top, deadline)?.text;
        }
        false => {
            let system = format!("You change files in a code repository. {FILES}");
            let ask = Ask { role, system: &system, prompt, sections, write: false, max_tokens: 4000 };
            e.reply = log.text(b, r, &ask, top, deadline)?;
            for (path, body) in parse_files(&e.reply) {
                let safe = !path.starts_with('/') && !path.split('/').any(|c| c == "..");
                if !(safe && allowed(&path)) {
                    e.reverted.push(path);
                    continue;
                }
                let full = top.join(&path);
                let d = full.parent().unwrap_or(top);
                std::fs::create_dir_all(d).map_err(|err| format!("{}: {err}", d.display()))?;
                std::fs::write(&full, body).map_err(|err| format!("{path}: {err}"))?;
            }
        }
    }
    let after = repo::worktree_tree(top)?;
    let changed = repo::changed(top, &before, &after)?;
    let (keep, out): (Vec<String>, Vec<String>) = changed.into_iter().partition(|p| allowed(p));
    repo::restore(top, &before, &out)?;
    e.changed = keep;
    e.reverted.extend(out);
    Ok(e)
}
