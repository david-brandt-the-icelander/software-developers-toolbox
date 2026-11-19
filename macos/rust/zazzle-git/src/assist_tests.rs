// Tests for the composing assistant, with a stand-in composer that other test modules use too.

use super::*;
use crate::agent::AGENTS;
use crate::compose::Answer;
use std::cell::RefCell;
use std::rc::Rc;

// What the stand-in composer was asked to do, and the answer it will give.
#[derive(Default)]
pub struct Fake {
    pub runs: Vec<(Vec<String>, String)>,
    pub answer: Option<Answer>,
    pub cancels: usize,
    pub remembered: Vec<(String, String)>,
}

// A composer that records runs and returns whatever answer the test sets.
pub struct FakeComposer(pub Rc<RefCell<Fake>>);

impl Composer for FakeComposer {
    // Records the run.
    fn start(&mut self, cmd: Vec<String>, prompt: String) {
        self.0.borrow_mut().runs.push((cmd, prompt));
    }
    // Hands out the answer once.
    fn poll(&mut self) -> Option<Answer> {
        self.0.borrow_mut().answer.take()
    }
    // Counts cancels.
    fn cancel(&mut self) {
        self.0.borrow_mut().cancels += 1;
    }
}

// Builds an assistant over the named agents with a stand-in composer, returning both.
pub fn fake(names: &[&str], saved: (&str, &str)) -> (Assist, Rc<RefCell<Fake>>) {
    let state = Rc::new(RefCell::new(Fake::default()));
    let agents = AGENTS.iter().filter(|a| names.contains(&a.name)).collect();
    let s = state.clone();
    let remember = Box::new(move |a: &str, e: &str| s.borrow_mut().remembered.push((a.into(), e.into())));
    let context = Box::new(|files: &[Entry]| format!("prompt for {} files", files.len()));
    let assist =
        Assist::new(agents, saved.0, saved.1, Box::new(FakeComposer(state.clone())), context, remember);
    (assist, state)
}

// Checks the starting choice: the saved agent and effort when found, else the first agent and its default.
#[test]
fn test_assist_new_choice() {
    let (a, _) = fake(&["claude", "codex"], ("codex", "high"));
    assert_eq!(
        (a.agent().map(|a| a.name), a.effort.as_str(), a.status.clone()),
        (Some("codex"), "high", Status::Idle)
    );
    let (b, _) = fake(&["claude"], ("gemini", "nope"));
    assert_eq!((b.agent().map(|a| a.name), b.effort.as_str()), (Some("claude"), "low"));
    let (none, _) = fake(&[], ("", ""));
    assert_eq!(none.status, Status::NoAgent);
    assert!(none.line(0).contains("No agent CLI found"));
}

// Checks a run: the prompt and command, the growing dots every 500 ms, then the suggestion or the failure.
#[test]
fn test_assist_run_and_dots() {
    let (mut a, state) = fake(&["claude"], ("", ""));
    assert!(a.line(0).contains("the message is yours"));
    a.start(&[], 1000);
    assert!(a.busy());
    assert_eq!(state.borrow().runs[0].1, "prompt for 0 files");
    assert_eq!(state.borrow().runs[0].0[0], "claude");
    assert!(a.tick(&[], 1000));
    assert!(a.line(1000).ends_with("claude is composing default commit message."));
    assert!(!a.tick(&[], 1200));
    assert!(a.tick(&[], 1500));
    assert!(a.line(2600).ends_with("message...."));
    state.borrow_mut().answer = Some(Ok("Fix the thing".into()));
    assert!(a.tick(&[], 2700));
    assert_eq!(a.suggestion(), Some("Fix the thing"));
    assert!(a.line(2700).contains("suggestion ready"));
    assert!(!a.tick(&[], 3000));
    assert!(!a.busy());
    a.start(&[], 4000);
    state.borrow_mut().answer = Some(Err("boom".into()));
    a.tick(&[], 4100);
    assert_eq!(a.status, Status::Failed("boom".into()));
    assert!(a.line(4100).contains("claude failed: boom"));
    assert_eq!(a.suggestion(), None);
}

// Checks cycling agents and efforts: each restarts the run and remembers the choice; single and missing agents say why not.
#[test]
fn test_assist_cycling() {
    let (mut a, state) = fake(&["claude", "codex", "gemini"], ("claude", "max"));
    assert_eq!(a.cycle_effort(&[], 0), "Effort: low");
    assert_eq!(a.cycle_agent(&[], 0), "Agent: codex");
    assert_eq!(a.effort, "low");
    assert_eq!(a.cycle_agent(&[], 0), "Agent: gemini");
    assert!(a.line(0).contains("effort n/a"));
    assert_eq!(a.cycle_effort(&[], 0), "gemini has no effort setting");
    assert_eq!(a.cycle_agent(&[], 0), "Agent: claude");
    let remembered = state.borrow().remembered.clone();
    let want: Vec<(String, String)> =
        [("claude", "low"), ("codex", "low"), ("gemini", ""), ("claude", "low")]
            .iter()
            .map(|(x, y)| (x.to_string(), y.to_string()))
            .collect();
    assert_eq!(remembered, want);
    assert_eq!(state.borrow().runs.len(), 4);
    let (mut one, _) = fake(&["codex"], ("", ""));
    assert_eq!(one.cycle_agent(&[], 0), "codex is the only agent CLI found");
    let (mut none, _) = fake(&[], ("", ""));
    assert!(none.cycle_agent(&[], 0).contains("No agent"));
    assert!(none.cycle_effort(&[], 0).contains("No agent"));
    none.start(&[], 0);
    assert_eq!(none.status, Status::NoAgent);
}

// Checks that mark changes start one new run after the pause, and nothing without an agent.
#[test]
fn test_assist_marks_changed_restarts() {
    let (mut a, state) = fake(&["claude"], ("", ""));
    a.marks_changed(1000);
    a.marks_changed(1200);
    assert!(a.busy());
    assert!(!a.tick(&[], 2000));
    assert!(a.tick(&[], 2700));
    assert_eq!(state.borrow().runs.len(), 1);
    let (mut none, _) = fake(&[], ("", ""));
    none.marks_changed(0);
    assert!(!none.busy());
}
