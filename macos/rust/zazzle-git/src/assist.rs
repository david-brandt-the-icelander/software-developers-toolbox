// The agent that composes the default commit message: which agent and effort are chosen, the run's progress and its suggestion.

use crate::agent::Agent;
use crate::compose::Composer;
use crate::files::Entry;

// How often a dot is added to the progress label.
pub const DOT_MS: u64 = 500;
// How long after the last file mark change a new run starts.
pub const RESTART_MS: u64 = 1500;

// Builds the prompt from the files and their marks.
pub type ContextFn = Box<dyn FnMut(&[Entry]) -> String>;
// Remembers the chosen agent and effort for the next run.
pub type RememberFn = Box<dyn FnMut(&str, &str)>;

// Where composing stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    NoAgent,
    Idle,
    Composing { since: u64 },
    Ready(String),
    Failed(String),
}

// The chosen agent and effort, the run's status, and the pieces that run it, build its prompt and remember the choice.
pub struct Assist {
    agents: Vec<&'static Agent>,
    agent: usize,
    pub effort: String,
    pub status: Status,
    pub save_waiting: bool,
    restart_at: Option<u64>,
    shown_dots: u64,
    composer: Box<dyn Composer>,
    context: ContextFn,
    remember: RememberFn,
}

impl Assist {
    // Creates the assistant with the agents found, preferring the saved agent and effort.
    pub fn new(
        agents: Vec<&'static Agent>,
        saved_agent: &str,
        saved_effort: &str,
        composer: Box<dyn Composer>,
        context: ContextFn,
        remember: RememberFn,
    ) -> Assist {
        let agent = agents.iter().position(|a| a.name == saved_agent).unwrap_or(0);
        let effort = agents.get(agent).map(|a| a.effort_or_default(saved_effort)).unwrap_or_default();
        let status = if agents.is_empty() { Status::NoAgent } else { Status::Idle };
        Assist {
            agents,
            agent,
            effort,
            status,
            save_waiting: false,
            restart_at: None,
            shown_dots: 0,
            composer,
            context,
            remember,
        }
    }

    // Returns the chosen agent, if any was found.
    pub fn agent(&self) -> Option<&'static Agent> {
        self.agents.get(self.agent).copied()
    }

    // Starts composing with the chosen agent from the marked files; does nothing when no agent was found.
    pub fn start(&mut self, files: &[Entry], now: u64) {
        let Some(agent) = self.agent() else { return };
        let prompt = (self.context)(files);
        self.composer.start(agent.command(&self.effort), prompt);
        self.status = Status::Composing { since: now };
        self.restart_at = None;
        self.shown_dots = 0;
    }

    // Switches to the next agent found, remembers it and starts again; returns what to tell the user.
    pub fn cycle_agent(&mut self, files: &[Entry], now: u64) -> String {
        match self.agents.len() {
            0 => return "No agent CLI found (claude, codex or gemini)".to_string(),
            1 => return format!("{} is the only agent CLI found", self.agents[0].name),
            n => self.agent = (self.agent + 1) % n,
        }
        let agent = self.agents[self.agent];
        self.effort = agent.effort_or_default(&self.effort);
        (self.remember)(agent.name, &self.effort);
        self.start(files, now);
        format!("Agent: {}", agent.name)
    }

    // Switches to the agent's next effort level, remembers it and starts again; returns what to tell the user.
    pub fn cycle_effort(&mut self, files: &[Entry], now: u64) -> String {
        let Some(agent) = self.agent() else {
            return "No agent CLI found (claude, codex or gemini)".to_string();
        };
        let Some(next) = agent.next_effort(&self.effort) else {
            return format!("{} has no effort setting", agent.name);
        };
        self.effort = next;
        (self.remember)(agent.name, &self.effort);
        self.start(files, now);
        format!("Effort: {}", self.effort)
    }

    // Plans a new run shortly, after the file marks changed, so a burst of changes starts only one run.
    pub fn marks_changed(&mut self, now: u64) {
        if self.agent().is_some() {
            self.restart_at = Some(now + RESTART_MS);
        }
    }

    // Returns true while something will happen without a key press: a run, a planned restart, or a save waiting for the answer.
    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Composing { .. }) || self.restart_at.is_some()
    }

    // Starts a planned run when due, takes in a finished run's answer, and adds dots; returns true if anything shown changed.
    pub fn tick(&mut self, files: &[Entry], now: u64) -> bool {
        if self.restart_at.is_some_and(|t| now >= t) {
            self.start(files, now);
            return true;
        }
        let Status::Composing { since } = self.status else { return false };
        if let Some(answer) = self.composer.poll() {
            self.status = match answer {
                Ok(text) => Status::Ready(text),
                Err(e) => Status::Failed(e),
            };
            return true;
        }
        let dots = (now.saturating_sub(since)) / DOT_MS + 1;
        let changed = dots != self.shown_dots;
        self.shown_dots = dots;
        changed
    }

    // Returns the suggestion once it is ready.
    pub fn suggestion(&self) -> Option<&str> {
        match &self.status {
            Status::Ready(s) => Some(s),
            _ => None,
        }
    }

    // Returns the agent line shown above the status line: agent, effort and what it is doing.
    pub fn line(&self, now: u64) -> String {
        let Some(agent) = self.agent() else {
            return "No agent CLI found (claude, codex or gemini): write the message yourself".to_string();
        };
        let effort = if self.effort.is_empty() { "n/a" } else { &self.effort };
        let state = match &self.status {
            Status::Composing { since } => {
                let dots = ".".repeat(((now.saturating_sub(*since)) / DOT_MS + 1) as usize);
                format!("{} is composing default commit message{dots}", agent.name)
            }
            Status::Ready(_) => "suggestion ready: Ctrl+S with an empty message uses it".to_string(),
            Status::Failed(e) => format!("{} failed: {e}", agent.name),
            Status::Idle | Status::NoAgent => {
                "the message is yours; Ctrl+N or Ctrl+E asks the agent".to_string()
            }
        };
        format!("{} · effort {effort} · {state}", agent.name)
    }

    // Stops a run in progress.
    pub fn cancel(&mut self) {
        self.composer.cancel();
    }
}

#[cfg(test)]
#[path = "assist_tests.rs"]
pub mod tests;
