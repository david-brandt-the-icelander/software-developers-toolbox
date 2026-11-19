// Runs an agent CLI in the background with the prompt on stdin, so the editor stays responsive; it can be polled and cancelled.

use crate::agent::clean;
use crate::git::drain;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// How long an agent may take before it is stopped.
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(300);

// An agent's answer, or why there is none.
pub type Answer = Result<String, String>;

// Something that composes a message in the background.
pub trait Composer {
    // Starts a new run with the command line and prompt, stopping any run still going.
    fn start(&mut self, cmd: Vec<String>, prompt: String);
    // Returns the answer once the run has finished.
    fn poll(&mut self) -> Option<Answer>;
    // Stops the current run, if any.
    fn cancel(&mut self);
}

// Runs the agent as a child process, with a timeout.
pub struct ProcessComposer {
    rx: Option<Receiver<Answer>>,
    child: Arc<Mutex<Option<Child>>>,
    timeout: Duration,
}

impl ProcessComposer {
    // Creates a composer whose runs are stopped after the timeout.
    pub fn new(timeout: Duration) -> ProcessComposer {
        ProcessComposer { rx: None, child: Arc::new(Mutex::new(None)), timeout }
    }
}

// Stops the agent and every process it started (it runs as the leader of its own process group), then reaps it.
fn kill_group(child: &mut Child) {
    // SAFETY: kill only sends a signal to the process group the child leads.
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let _ = child.kill();
    let _ = child.wait();
}

// Waits for the shared child to finish or time out; returns None if it was cancelled (taken away) meanwhile.
fn wait(shared: &Mutex<Option<Child>>, timeout: Duration) -> Option<Result<bool, String>> {
    let start = Instant::now();
    loop {
        let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
        let child = guard.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                guard.take();
                return Some(Ok(status.success()));
            }
            Ok(None) if start.elapsed() > timeout => {
                kill_group(child);
                guard.take();
                return Some(Err(format!("no answer after {} s", timeout.as_secs())));
            }
            Ok(None) => {}
            Err(e) => return Some(Err(e.to_string())),
        }
        drop(guard);
        thread::sleep(Duration::from_millis(20));
    }
}

impl Composer for ProcessComposer {
    // Spawns the agent and a thread that feeds it the prompt and sends back its answer.
    fn start(&mut self, cmd: Vec<String>, prompt: String) {
        self.cancel();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let spawned = Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(format!("cannot run {}: {e}", cmd[0])));
                return;
            }
        };
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let out = drain(child.stdout.take().expect("stdout is piped"));
        let err = drain(child.stderr.take().expect("stderr is piped"));
        self.child = Arc::new(Mutex::new(Some(child)));
        let shared = self.child.clone();
        let timeout = self.timeout;
        thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
            drop(stdin);
            let Some(result) = wait(&shared, timeout) else { return };
            let out = out.join().unwrap_or_default();
            let err = err.join().unwrap_or_default();
            let answer = match result {
                Ok(true) if !clean(&out).is_empty() => Ok(clean(&out)),
                Ok(true) => Err("the agent gave an empty answer".to_string()),
                Ok(false) => {
                    Err(err.lines().find(|l| !l.trim().is_empty()).unwrap_or("the agent failed").to_string())
                }
                Err(e) => Err(e),
            };
            let _ = tx.send(answer);
        });
    }

    // Returns the answer if the run has finished.
    fn poll(&mut self) -> Option<Answer> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok(a) => {
                self.rx = None;
                Some(a)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                None
            }
        }
    }

    // Kills the running agent and forgets its answer.
    fn cancel(&mut self) {
        self.rx = None;
        if let Some(mut c) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            kill_group(&mut c);
        }
    }
}

impl Drop for ProcessComposer {
    // Stops the agent when the editor closes.
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Polls until an answer arrives or a few seconds pass.
    fn answer(c: &mut ProcessComposer) -> Option<Answer> {
        for _ in 0..500 {
            if let Some(a) = c.poll() {
                return Some(a);
            }
            thread::sleep(Duration::from_millis(10));
        }
        None
    }

    // Turns words into a command line.
    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    // Checks a successful run that echoes the prompt, a failing one, an empty answer and a missing program.
    #[test]
    fn test_process_composer_answers() {
        let mut c = ProcessComposer::new(AGENT_TIMEOUT);
        assert_eq!(c.poll(), None);
        c.start(sh("cat"), "```\nSubject\n```".into());
        assert_eq!(answer(&mut c), Some(Ok("Subject".into())));
        c.start(sh("echo nope >&2; exit 2"), String::new());
        assert_eq!(answer(&mut c), Some(Err("nope".into())));
        c.start(sh("exit 3"), String::new());
        assert_eq!(answer(&mut c), Some(Err("the agent failed".into())));
        c.start(sh("true"), String::new());
        assert_eq!(answer(&mut c), Some(Err("the agent gave an empty answer".into())));
        c.start(vec!["/nonexistent/agent".into()], String::new());
        assert!(answer(&mut c).is_some_and(|a| a.is_err_and(|e| e.starts_with("cannot run"))));
    }

    // Checks the timeout, and that cancelling or restarting drops the earlier run's answer.
    #[test]
    fn test_process_composer_timeout_and_cancel() {
        let mut c = ProcessComposer::new(Duration::from_millis(100));
        c.start(sh("sleep 5"), String::new());
        assert!(answer(&mut c).is_some_and(|a| a.is_err_and(|e| e.contains("no answer"))));
        let mut d = ProcessComposer::new(AGENT_TIMEOUT);
        d.start(sh("sleep 5; echo late"), String::new());
        d.cancel();
        assert_eq!(d.poll(), None);
        d.start(sh("sleep 5; echo first"), String::new());
        d.start(sh("echo second"), String::new());
        assert_eq!(answer(&mut d), Some(Ok("second".into())));
        d.start(sh("sleep 5"), String::new());
        drop(d);
    }
}
