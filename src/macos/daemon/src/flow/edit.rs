// How delegated agents change files, alone or several at once with separate scopes: a hosted agent edits the working tree itself (allowed to write, never to run commands); a local model replies with whole files (`FILE path` then a fenced block) that zazzle writes; either way every change outside the paths it was given is put back, and the paths it changed are returned.
use super::repo;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{self, Ask, Backend, Log};
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

// One edit among others run at the same time: the agent, its role, the prompt, the named sections sent and the paths it may change.
pub struct Job<'a> {
    pub r: &'a Rung,
    pub role: &'a str,
    pub prompt: &'a str,
    pub sections: &'a [(&'a str, &'a str)],
    pub allowed: &'a (dyn Fn(&str) -> bool + Sync),
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
    allowed: &(dyn Fn(&str) -> bool + Sync),
    deadline: Instant,
) -> Result<Edited, String> {
    let job = Job { r, role, prompt, sections, allowed };
    together(top, b, log, std::slice::from_ref(&job), deadline)?
        .pop()
        .unwrap_or_else(|| Err("no edit".into()))
}

// Runs edits with separate scopes at the same time and returns each one's result in order. The agents are asked together; a hosted agent writes the tree itself, so at most one may be hosted (more run one after another), and its changes outside its scope are put back; local models' files are then written one job at a time, each kept only inside its own scope, so every change is attributed to the agent that made it.
pub fn together(
    top: &Path,
    b: &dyn Backend,
    log: &Log,
    jobs: &[Job],
    deadline: Instant,
) -> Result<Vec<Result<Edited, String>>, String> {
    if jobs.len() > 1 && jobs.iter().filter(|j| j.r.hosted()).count() > 1 {
        return Ok(jobs
            .iter()
            .map(|j| edit(top, b, log, j.r, j.role, j.prompt, j.sections, j.allowed, deadline))
            .collect());
    }
    let before = repo::worktree_tree(top)?;
    let replies = calls::all(jobs, |_, j| match j.r.hosted() {
        true => {
            let ask = Ask {
                role: j.role,
                system: "",
                prompt: j.prompt,
                sections: j.sections,
                write: true,
                max_tokens: 0,
            };
            log.hosted(b, j.r, &ask, top, deadline).map(|s| s.text)
        }
        false => {
            let system = format!("You change files in a code repository. {FILES}");
            let ask = Ask {
                role: j.role,
                system: &system,
                prompt: j.prompt,
                sections: j.sections,
                write: false,
                max_tokens: 4000,
            };
            log.text(b, j.r, &ask, top, deadline)
        }
    });
    let mut tree = repo::worktree_tree(top)?;
    let mut out: Vec<Option<Result<Edited, String>>> = jobs.iter().map(|_| None).collect();
    let mut replies: Vec<Option<Result<String, String>>> = replies.into_iter().map(Some).collect();
    let order = (0..jobs.len())
        .filter(|&i| jobs[i].r.hosted())
        .chain((0..jobs.len()).filter(|&i| !jobs[i].r.hosted()));
    for i in order.collect::<Vec<_>>() {
        let (j, reply) = (&jobs[i], replies[i].take().unwrap_or_else(|| Err("no reply".into())));
        out[i] = Some(match j.r.hosted() {
            true => {
                let (keep, back) = settle(top, &before, &tree, j.allowed)?;
                tree = repo::worktree_tree(top)?;
                reply.map(|reply| Edited { changed: keep, reverted: back, reply })
            }
            false => match reply {
                Err(e) => Err(e),
                Ok(reply) => {
                    let mut e = Edited { reply, ..Edited::default() };
                    for (path, body) in parse_files(&e.reply) {
                        let safe = !path.starts_with('/') && !path.split('/').any(|c| c == "..");
                        if !(safe && (j.allowed)(&path)) {
                            e.reverted.push(path);
                            continue;
                        }
                        let full = top.join(&path);
                        let d = full.parent().unwrap_or(top);
                        std::fs::create_dir_all(d).map_err(|err| format!("{}: {err}", d.display()))?;
                        std::fs::write(&full, body).map_err(|err| format!("{path}: {err}"))?;
                    }
                    let (keep, back) = settle(top, &tree, &repo::worktree_tree(top)?, j.allowed)?;
                    tree = repo::worktree_tree(top)?;
                    e.changed = keep;
                    e.reverted.extend(back);
                    Ok(e)
                }
            },
        });
    }
    Ok(out.into_iter().map(|o| o.unwrap_or_else(|| Err("no edit".into()))).collect())
}

// Splits the paths changed from `from` to `to` into those `allowed` accepts and the rest, which are put back as they were in `from`.
fn settle(
    top: &Path,
    from: &str,
    to: &str,
    allowed: &dyn Fn(&str) -> bool,
) -> Result<(Vec<String>, Vec<String>), String> {
    let (keep, back): (Vec<String>, Vec<String>) =
        repo::changed(top, from, to)?.into_iter().partition(|p| allowed(p));
    repo::restore(top, from, &back)?;
    Ok((keep, back))
}
