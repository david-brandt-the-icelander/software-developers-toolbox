// Builds Qwen3.5 ChatML prompt text and splits replies into reasoning and answer.

const THINK_END: &str = "</think>";

// One finished exchange kept so the prompt can be rebuilt when the context fills.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub user: String,
    pub answer: String,
}

// Opens the assistant reply, either letting the model think or skipping it as the template does.
fn assistant_open(think: bool) -> &'static str {
    if think {
        "<|im_start|>assistant\n<think>\n"
    } else {
        "<|im_start|>assistant\n<think>\n\n</think>\n\n"
    }
}

// Text that starts a conversation: the system message, if any.
pub fn system(system: &str) -> String {
    if system.is_empty() {
        return String::new();
    }
    format!("<|im_start|>system\n{system}<|im_end|>\n")
}

// Text that appends a user message after a reply (or the system text) already in the context.
pub fn user_turn(user: &str, after_reply: bool, think: bool) -> String {
    let close = if after_reply { "<|im_end|>\n" } else { "" };
    format!("{close}<|im_start|>user\n{user}<|im_end|>\n{}", assistant_open(think))
}

// Whole prompt for a fresh context: system, past turns without their reasoning, then the new message.
pub fn full(sys: &str, history: &[Turn], user: &str, think: bool) -> String {
    let mut out = system(sys);
    for t in history {
        out.push_str(&format!(
            "<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n{}<|im_end|>\n",
            t.user, t.answer
        ));
    }
    out + &user_turn(user, false, think)
}

// Part of a streamed reply: the model's reasoning or its answer.
#[derive(Debug, PartialEq)]
pub enum Segment {
    Reasoning(String),
    Answer(String),
}

// Splits streamed reply text at </think>, holding back a possible partial tag.
pub struct Splitter {
    thinking: bool,
    held: String,
    answer: String,
}

impl Splitter {
    // Starts inside the reasoning block when thinking is on.
    pub fn new(think: bool) -> Self {
        Splitter { thinking: think, held: String::new(), answer: String::new() }
    }

    // Takes the next piece of text and returns what can be shown now.
    pub fn push(&mut self, piece: &str) -> Vec<Segment> {
        self.held.push_str(piece);
        let mut out = Vec::new();
        if self.thinking {
            if let Some(i) = self.held.find(THINK_END) {
                let reasoning = self.held[..i].to_string();
                self.held = self.held[i + THINK_END.len()..].trim_start().to_string();
                self.thinking = false;
                if !reasoning.is_empty() {
                    out.push(Segment::Reasoning(reasoning));
                }
            } else {
                let keep = partial_suffix(&self.held, THINK_END);
                let ready: String = self.held.drain(..self.held.len() - keep).collect();
                if !ready.is_empty() {
                    out.push(Segment::Reasoning(ready));
                }
                return out;
            }
        }
        if !self.held.is_empty() {
            let text = std::mem::take(&mut self.held);
            self.answer.push_str(&text);
            out.push(Segment::Answer(text));
        }
        out
    }

    // The answer text so far, without reasoning, trimmed.
    pub fn answer(&self) -> String {
        self.answer.trim().to_string()
    }
}

// Length of the longest end of text that is a start of tag.
fn partial_suffix(text: &str, tag: &str) -> usize {
    (1..tag.len()).rev().find(|&n| text.ends_with(&tag[..n])).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // An empty system message adds nothing.
    #[test]
    fn test_system_empty() {
        assert_eq!(system(""), "");
        assert_eq!(system("Be brief."), "<|im_start|>system\nBe brief.<|im_end|>\n");
    }

    // A follow-up message closes the previous reply first.
    #[test]
    fn test_user_turn_after_reply() {
        let t = user_turn("hi", true, false);
        assert!(t.starts_with("<|im_end|>\n<|im_start|>user\nhi<|im_end|>\n"));
        assert!(t.ends_with("<think>\n\n</think>\n\n"));
        assert!(user_turn("hi", false, true).ends_with("assistant\n<think>\n"));
    }

    // A rebuilt prompt replays past answers and ends with the new message.
    #[test]
    fn test_full_with_history() {
        let h = vec![Turn { user: "a".into(), answer: "b".into() }];
        let p = full("s", &h, "c", false);
        assert_eq!(
            p,
            "<|im_start|>system\ns<|im_end|>\n<|im_start|>user\na<|im_end|>\n<|im_start|>assistant\nb<|im_end|>\n<|im_start|>user\nc<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        );
    }

    // Without thinking, every piece is answer text.
    #[test]
    fn test_splitter_no_think() {
        let mut s = Splitter::new(false);
        assert_eq!(s.push("Hel"), vec![Segment::Answer("Hel".into())]);
        assert_eq!(s.push("lo "), vec![Segment::Answer("lo ".into())]);
        assert_eq!(s.answer(), "Hello");
    }

    // A tag split across pieces is held back and then switches to the answer.
    #[test]
    fn test_splitter_split_tag() {
        let mut s = Splitter::new(true);
        assert_eq!(s.push("hmm</th"), vec![Segment::Reasoning("hmm".into())]);
        assert_eq!(s.push("ink>\n\nYes"), vec![Segment::Answer("Yes".into())]);
        assert_eq!(s.push("!"), vec![Segment::Answer("!".into())]);
        assert_eq!(s.answer(), "Yes!");
    }

    // A tag arriving as one piece with no reasoning before it yields only the answer.
    #[test]
    fn test_splitter_whole_tag() {
        let mut s = Splitter::new(true);
        assert_eq!(s.push("</think>"), vec![]);
        assert_eq!(s.push("Ok"), vec![Segment::Answer("Ok".into())]);
    }

    // The held-back length matches only real tag prefixes.
    #[test]
    fn test_partial_suffix() {
        assert_eq!(partial_suffix("ab</", THINK_END), 2);
        assert_eq!(partial_suffix("ab", THINK_END), 0);
        assert_eq!(partial_suffix("<", THINK_END), 1);
    }
}
