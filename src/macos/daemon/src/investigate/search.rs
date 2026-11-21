// How a FIND lookup's argument is read into the text to find, the path to search inside and the case rule, and where it may search, so the model's legacy forms run as meant or fail with a syntax error before anything is searched.
use super::{cite, Plan};
use crate::cmd::Ctx;
use std::path::{Path, PathBuf};

// A FIND as zazzle runs it: the text to look for, the path to search inside (None for the whole scope) and whether case must match (only for text with capitals, so `idempotency` finds `IdempotencyKeyFilter`).
#[derive(Debug, PartialEq)]
pub struct Search {
    pub needle: String,
    pub within: Option<String>,
    pub exact: bool,
}

// Whether the last word of an unquoted FIND names where to search rather than what.
fn place(w: &str) -> bool {
    w == "."
        || w.starts_with("./")
        || w.ends_with('/')
        || (w.contains('/') && !w.contains(['(', '"', '<', '=']))
}

// Reads a FIND's argument into the text, the path and the case rule, so a quoted literal never keeps its quotes or the path after it: `FIND text`, `FIND text path`, `FIND "quoted text" [in] [path]` (case matters only for text with capitals); a quote left open, nothing to find or more than a path after the quoted text is a syntax error, said before anything is searched.
pub fn search(raw: &str) -> Result<Search, String> {
    let raw = raw.trim();
    let syntax = |why: &str| {
        format!("FIND syntax: {why}; nothing was searched. Write FIND text (any case unless it has capitals), FIND \"text with spaces\", or either followed by a path inside the scope")
    };
    let (needle, rest) = match raw.chars().next() {
        Some(q @ ('"' | '\'' | '`')) => {
            let body = &raw[1..];
            let Some(end) = body.find(q) else { return Err(syntax("the quote is not closed")) };
            (body[..end].to_string(), body[end + 1..].trim())
        }
        _ => {
            let words: Vec<&str> = raw.split_whitespace().collect();
            match words.split_last() {
                Some((last, head)) if !head.is_empty() && place(last) => {
                    let head = head.strip_suffix(&["in"]).unwrap_or(head);
                    (head.join(" "), *last)
                }
                _ => (raw.to_string(), ""),
            }
        }
    };
    if needle.trim().is_empty() {
        return Err(syntax("there is no text to find"));
    }
    let rest = rest.strip_prefix("in ").unwrap_or(rest).trim().trim_matches(['"', '\'', '`']);
    if rest.contains(char::is_whitespace) && !Path::new(rest).exists() {
        return Err(syntax(&format!("only a path may follow the quoted text, not `{rest}`")));
    }
    let exact = needle.chars().any(char::is_uppercase);
    Ok(Search { needle, within: (!rest.is_empty()).then(|| rest.to_string()), exact })
}

impl Search {
    // The search as zazzle ran it, for the record: the text, the case rule and where.
    pub fn describe(&self) -> serde_json::Value {
        serde_json::json!({"text": self.needle, "case": if self.exact { "exact" } else { "any" }, "within": self.within.as_deref().unwrap_or("the scope")})
    }
}

// The paths a FIND with a path searches: that path when it lies inside the scope, the whole scope when the path holds it (such as `.`), else a refusal.
pub fn within(lctx: &Ctx, scope: &[PathBuf], plan: &Plan, p: &str) -> Result<Vec<String>, String> {
    let Ok(full) = std::fs::canonicalize(lctx.cwd.join(p)) else {
        return Err(format!(
            "refused: FIND path {p} does not exist inside the scope ({})",
            plan.scope.join(", ")
        ));
    };
    match (cite::in_scope(&full, scope), scope.iter().any(|s| s.starts_with(&full))) {
        (true, _) => Ok(vec![p.to_string()]),
        (false, true) => Ok(plan.scope.clone()),
        (false, false) => {
            Err(format!("refused: FIND path {p} is outside the scope ({})", plan.scope.join(", ")))
        }
    }
}
