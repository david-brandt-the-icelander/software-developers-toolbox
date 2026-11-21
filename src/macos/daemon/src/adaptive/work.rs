// The structured result every delegated call in the adaptive mode returns, checked by zazzle: status, how sure the executor is, verified findings, gaps and its request to escalate, the coverage of the items it was asked, plus the triggers that hand it to a stronger agent.
use crate::investigate::{cite, verify, Found};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

// What one executor returned, checked: status (complete, partial or failed), its understanding of the task and the
// support for its results (high, medium, low or empty when not given), findings whose citations zazzle verified, the
// citations it rejected, gaps, the question it would escalate with context, what only the user can give, turns, the
// answer's lines, the lookups it made (`LOOKUP` or `LOOKUP — found nothing: WHY`), what the run did, each item's
// coverage, the citations about no item and the lookups that would settle what is missing.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Work {
    pub status: String,
    pub understanding: String,
    pub confidence: String,
    pub findings: Vec<Found>,
    pub rejected: Vec<String>,
    pub gaps: Vec<String>,
    pub escalate: Option<String>,
    pub context: Vec<String>,
    pub blocked: Option<String>,
    pub turns: usize,
    pub lines: Vec<Line>,
    pub tried: Vec<String>,
    pub diagnostics: Value,
    pub coverage: Vec<super::coverage::Cover>,
    pub unrelated: Vec<String>,
    pub follow: Vec<String>,
}

// One line of a synthesized answer: what kind of statement it is (supported, inference, proposal or unknown), its words, and the findings it rests on (indexes into the findings).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Line {
    pub kind: String,
    pub text: String,
    pub ids: Vec<usize>,
}

impl Work {
    // A failed result that says why.
    pub fn failed(why: &str) -> Work {
        Work { status: "failed".into(), gaps: vec![why.to_string()], ..Work::default() }
    }

    // The checked result of a reply in the required lines: every FINDING's citation verified inside the scope; a reply with none of the lines fails.
    pub fn from_reply(cwd: &Path, scope: &[PathBuf], text: &str) -> Work {
        let Some(a) = cite::parse_answer(text) else {
            return Work::failed(&format!(
                "the reply had none of the required lines (it began: {})",
                super::agent::one_line(&text.chars().take(160).collect::<String>())
            ));
        };
        let mut w = Work {
            understanding: a.understanding.clone(),
            confidence: a.confidence.clone(),
            gaps: a.unknowns.clone(),
            escalate: a.escalate.clone(),
            context: a.context.clone(),
            blocked: a.blocked.clone(),
            turns: 1,
            ..Work::default()
        };
        for c in &a.findings {
            let (f, r) = verify(cwd, scope, c);
            w.findings.extend(f);
            w.rejected.extend(r);
        }
        w.status = match (a.status.as_str(), w.findings.is_empty()) {
            ("", true) => "failed".into(),
            ("", false) => "complete".into(),
            ("complete", true) => "partial".into(),
            (s, _) => s.to_string(),
        };
        w
    }

    // Why this result's open question should go to a stronger agent, or None: a failure, a request to escalate, a
    // low rating, citations that all failed, nothing verified or an item left unresolved, however sure the executor
    // was; never when only the user can unblock it.
    pub fn trigger(&self) -> Option<String> {
        if self.blocked.is_some() {
            return None;
        }
        let first = self.gaps.first().map(|g| format!(": {g}")).unwrap_or_default();
        let open = self
            .coverage
            .iter()
            .find(|c| !c.missing.is_empty())
            .map(|c| format!("it left {} unresolved: {}", c.id, c.missing[0]));
        Some(match () {
            _ if self.status == "failed" => format!("it failed{first}"),
            _ if self.escalate.is_some() => "it asked to escalate".into(),
            _ if self.understanding == "low" => "it rated its understanding of the task low".into(),
            _ if self.confidence == "low" => "it rated the support for its results low".into(),
            _ if self.findings.is_empty() && !self.rejected.is_empty() => {
                "none of its citations checked out".into()
            }
            _ if self.findings.is_empty() => "it verified nothing".into(),
            _ => return open,
        })
    }

    // What the next agent gets after this attempt: the verified findings, what failed, each item still unresolved, the citations about no item, the lookups zazzle runs for it first and the executor's own context.
    pub fn handoff(&self, who: &str, why: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .findings
            .iter()
            .filter(|f| !self.unrelated.contains(&f.cite))
            .map(|f| format!("verified: {} ({})", f.claim, f.cite))
            .collect();
        out.push(format!("{who} did not settle it: {why}"));
        out.extend(self.rejected.iter().map(|r| format!("rejected citation: {r}")));
        out.extend(self.missing().into_iter().map(|m| format!("unresolved: {m}")));
        out.extend(self.unrelated.iter().map(|u| format!("about none of the items (look elsewhere): {u}")));
        out.extend(self.gaps.iter().take(4).map(|g| format!("open: {g}")));
        out.extend(self.follow.iter().map(|l| format!("zazzle looks up next: {l}")));
        out.extend(self.context.iter().cloned());
        out
    }

    // Each item still unresolved, as `I2 (its words): what it lacks`.
    pub fn missing(&self) -> Vec<String> {
        self.coverage
            .iter()
            .flat_map(|c| c.missing.iter().map(move |m| format!("{} ({}): {m}", c.id, c.text)))
            .collect()
    }

    // Lowers a complete status to partial when an item is unresolved.
    pub fn hold_to_coverage(&mut self) {
        if self.status == "complete" && !self.missing().is_empty() {
            self.status = "partial".into();
        }
    }

    // Keeps each verified finding in the job's evidence as soon as the agent `by` returns it, so a run that stops before its answer still has them.
    pub fn checkpoint(&self, by: &str) {
        for f in &self.findings {
            crate::investigate::cache::checkpoint(json!({"finding": f.claim, "citation": f.cite, "by": by}));
        }
    }

    // The structured result for a caller: status, the two levels, findings with evidence, gaps, the escalation and what only the user can give.
    pub fn json(&self) -> Value {
        let findings: Vec<Value> = self
            .findings
            .iter()
            .map(|f| json!({"claim": f.claim, "evidence": [f.cite], "excerpt": f.excerpt}))
            .collect();
        let level = |s: &str| if s.is_empty() { Value::Null } else { s.into() };
        json!({
            "answer": self.answer(),
            "status": self.status,
            "task_understanding": level(&self.understanding),
            "result_confidence": level(&self.confidence),
            "findings": findings,
            "rejected": self.rejected,
            "gaps": self.gaps,
            "escalation": {
                "recommended": self.trigger().is_some(),
                "question": self.escalate,
                "context": self.context,
            },
            "needs_user": self.blocked,
            "coverage": self.coverage.iter().map(|c| json!({
                "item": c.id, "text": c.text, "covered": c.missing.is_empty(), "evidence": c.findings, "missing": c.missing,
            })).collect::<Vec<_>>(),
            "unrelated_citations": self.unrelated,
            "diagnostics": self.diagnostics,
        })
    }

    // The answer as the caller asked for it: each verified finding with its citation and the exact lines it rests on, then what is still unresolved, as text and as fields.
    pub fn answer(&self) -> Value {
        let mut text: String = match self.lines.is_empty() {
            true => self
                .findings
                .iter()
                .map(|f| match self.unrelated.contains(&f.cite) {
                    true => format!("- About none of the items: {} ({})\n", f.claim, f.cite),
                    false => format!("- {} ({})\n", f.claim, f.cite),
                })
                .collect(),
            false => self.lines.iter().map(|l| format!("- {}\n", self.line_text(l))).collect(),
        };
        let unresolved: Vec<String> = self.missing().into_iter().chain(self.gaps.iter().cloned()).collect();
        if !unresolved.is_empty() {
            text.push_str("Unresolved:\n");
            text.extend(unresolved.iter().map(|g| format!("- {g}\n")));
        }
        json!({
            "text": text,
            "lines": self.lines.iter().map(|l| json!({
                "kind": l.kind,
                "text": l.text,
                "findings": l.ids.iter().map(|i| json!({"id": format!("F{}", i + 1), "citation": self.findings[*i].cite})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "findings": self.findings.iter().enumerate().map(|(i, f)| json!({"id": format!("F{}", i + 1), "claim": f.claim, "citation": f.cite, "excerpt": f.excerpt})).collect::<Vec<_>>(),
            "unresolved": unresolved,
        })
    }

    // One answer line as text: a label for what is not supported, its words, and the citations of its findings.
    pub fn line_text(&self, l: &Line) -> String {
        let label = match l.kind.as_str() {
            "supported" => "",
            "inference" => "Inference: ",
            "proposal" => "Proposal: ",
            _ => "Unknown: ",
        };
        let cites: Vec<String> =
            l.ids.iter().map(|i| format!("[{}] {}", i + 1, self.findings[*i].cite)).collect();
        match cites.is_empty() {
            true => format!("{label}{}", l.text),
            false => format!("{label}{} ({})", l.text, cites.join("; ")),
        }
    }

    // Renders the findings with their cited lines and the rest of the result, for the text report.
    pub fn render(&self) -> String {
        let mut out: String = self.lines.iter().map(|l| format!("- {}\n", self.line_text(l))).collect();
        out.push_str(&format!("findings ({} verified):\n", self.findings.len()));
        if self.findings.is_empty() {
            out.push_str("(none)\n");
        }
        for (i, f) in self.findings.iter().enumerate() {
            out.push_str(&format!("{}. {}\n   {}\n{}", i + 1, f.claim, f.cite, f.excerpt));
        }
        let list = |title: &str, items: &[String]| match items.is_empty() {
            true => String::new(),
            false => format!("{title}:\n{}", items.iter().map(|i| format!("- {i}\n")).collect::<String>()),
        };
        out.push_str(&list("rejected citations", &self.rejected));
        out.push_str(&list("gaps", &self.gaps));
        if !self.coverage.is_empty() {
            let open = self.coverage.iter().filter(|c| !c.missing.is_empty()).count();
            out.push_str(&format!(
                "coverage: {} of {} items covered\n",
                self.coverage.len() - open,
                self.coverage.len()
            ));
            out.push_str(&list("unresolved items", &self.missing()));
            out.push_str(&list("citations about none of the items", &self.unrelated));
        }
        let or = |s: &str| if s.is_empty() { "not given".to_string() } else { s.to_string() };
        out.push_str(&format!(
            "confidence: understanding {}, result {}\n",
            or(&self.understanding),
            or(&self.confidence)
        ));
        if let Some(q) = &self.escalate {
            out.push_str(&format!("escalate: {q}\n"));
        }
        if let Some(b) = &self.blocked {
            out.push_str(&format!("needs the user: {b}\n"));
        }
        out
    }

    // A compact listing for another model's prompt, tagged (`A1 path:A-B claim`), with the first cited lines when `lines` > 0, and the gaps.
    pub fn listing(&self, tag: &str, lines: usize) -> String {
        let mut out = String::new();
        for (i, f) in self.findings.iter().enumerate() {
            out.push_str(&format!("{tag}{} {} {}\n", i + 1, f.cite, f.claim));
            for l in f.excerpt.lines().take(lines) {
                out.push_str(&format!("    {}\n", l.trim_end()));
            }
        }
        if self.findings.is_empty() {
            out.push_str("(no verified findings)\n");
        }
        out.extend(self.gaps.iter().map(|g| format!("GAP {g}\n")));
        out
    }

    // One line for tables: status, verified findings and levels.
    pub fn summary(&self) -> String {
        let or = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        format!(
            "{}, {} verified, understanding {}, result {}",
            self.status,
            self.findings.len(),
            or(&self.understanding),
            or(&self.confidence)
        )
    }
}

// The lowest of two levels, an empty one counting as unknown and losing to any given level.
pub fn lowest(a: &str, b: &str) -> String {
    let rank = |s: &str| ["low", "medium", "high"].iter().position(|l| *l == s);
    match (rank(a), rank(b)) {
        (Some(x), Some(y)) if y < x => b.to_string(),
        (Some(_), _) => a.to_string(),
        (None, _) => b.to_string(),
    }
}
