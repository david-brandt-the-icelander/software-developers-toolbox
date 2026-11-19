// The agent CLIs that can compose a commit message (claude, codex, gemini): finding them on PATH and the command line for each.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;

// One agent CLI: its program name, its effort levels (empty when it has none) and the level used when none is saved.
#[derive(Debug, PartialEq, Eq)]
pub struct Agent {
    pub name: &'static str,
    pub efforts: &'static [&'static str],
    pub default_effort: &'static str,
}

// The agents looked for, in the order they are offered.
pub const AGENTS: [Agent; 3] = [
    Agent { name: "claude", efforts: &["low", "medium", "high", "xhigh", "max"], default_effort: "low" },
    Agent { name: "codex", efforts: &["low", "medium", "high"], default_effort: "low" },
    Agent { name: "gemini", efforts: &[], default_effort: "" },
];

impl Agent {
    // Returns the command line that reads the prompt on stdin and prints only the answer, without tools or file edits.
    pub fn command(&self, effort: &str) -> Vec<String> {
        let words: Vec<String> = match self.name {
            "claude" => ["claude", "-p", "--tools", "", "--no-session-persistence", "--effort", effort]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            "codex" => vec![
                "codex".into(),
                "exec".into(),
                "--sandbox".into(),
                "read-only".into(),
                "--skip-git-repo-check".into(),
                "--ephemeral".into(),
                "-c".into(),
                format!("model_reasoning_effort={effort}"),
                "-".into(),
            ],
            _ => ["gemini", "--approval-mode", "plan", "-p", "Write the commit message described above."]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        words
    }

    // Returns the saved effort if this agent has it, else the agent's default.
    pub fn effort_or_default(&self, saved: &str) -> String {
        if self.efforts.contains(&saved) {
            saved.to_string()
        } else {
            self.default_effort.to_string()
        }
    }

    // Returns the effort after the given one, wrapping around, or None when the agent has no effort levels.
    pub fn next_effort(&self, current: &str) -> Option<String> {
        let at = self.efforts.iter().position(|e| *e == current)?;
        Some(self.efforts[(at + 1) % self.efforts.len()].to_string())
    }
}

// Returns the agents whose program is an executable file in one of the PATH directories.
pub fn available(path: &OsStr) -> Vec<&'static Agent> {
    let dirs: Vec<_> = std::env::split_paths(path).collect();
    AGENTS
        .iter()
        .filter(|a| {
            dirs.iter().any(|d| {
                std::fs::metadata(d.join(a.name))
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
        })
        .collect()
}

// Returns the agent's answer without surrounding whitespace or a Markdown code fence around it.
pub fn clean(answer: &str) -> String {
    let lines: Vec<&str> = answer.trim().lines().collect();
    let fenced = lines.len() >= 2 && lines[0].starts_with("```") && lines[lines.len() - 1].trim() == "```";
    let body = if fenced { &lines[1..lines.len() - 1] } else { &lines[..] };
    body.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks each agent's command line and effort handling.
    #[test]
    fn test_agent_commands_and_efforts() {
        let [claude, codex, gemini] = &AGENTS;
        assert_eq!(claude.command("high")[..4], ["claude", "-p", "--tools", ""]);
        assert!(claude.command("high").ends_with(&["--effort".to_string(), "high".to_string()]));
        assert!(codex.command("medium").contains(&"model_reasoning_effort=medium".to_string()));
        assert_eq!(gemini.command("")[0], "gemini");
        assert_eq!(claude.effort_or_default("max"), "max");
        assert_eq!(codex.effort_or_default("max"), "low");
        assert_eq!(gemini.effort_or_default("low"), "");
        assert_eq!(claude.next_effort("max").as_deref(), Some("low"));
        assert_eq!(codex.next_effort("low").as_deref(), Some("medium"));
        assert_eq!(gemini.next_effort(""), None);
    }

    // Checks finding agents on PATH: executable files only, in the fixed order.
    #[test]
    fn test_available() {
        let d = std::env::temp_dir().join(format!("zg-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("codex")).unwrap();
        for (name, mode) in [("gemini", 0o755), ("claude", 0o644)] {
            std::fs::write(d.join(name), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(d.join(name), std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let path = std::env::join_paths([d.clone(), d.join("missing")]).unwrap();
        let names: Vec<&str> = available(&path).iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["gemini"]);
        std::fs::set_permissions(d.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let names: Vec<&str> = available(&path).iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["claude", "gemini"]);
        std::fs::remove_dir_all(d).unwrap();
    }

    // Checks trimming and removing a code fence.
    #[test]
    fn test_clean() {
        assert_eq!(clean("  Fix it\n\nBody\n"), "Fix it\n\nBody");
        assert_eq!(clean("```text\nFix it\n```\n"), "Fix it");
        assert_eq!(clean("```"), "```");
        assert_eq!(clean(""), "");
    }
}
