// Splits streamed model text into thinking and answer, across the thinking markers of the model families in the catalogue.

// Thinking spans as (open, close) markers: Qwen, Gemma 4 and gpt-oss's analysis channel.
const SPANS: [(&str, &str); 3] = [
    ("<think>", "</think>"),
    ("<|channel>thought", "<channel|>"),
    ("<|channel|>analysis<|message|>", "<|end|>"),
];

// Markers dropped from the answer: the headers of gpt-oss's next message and final channel.
const DROP: [&str; 2] = ["<|channel|>final<|message|>", "<|start|>assistant"];

// One piece of output: part of the answer or part of the thinking.
#[derive(Debug, PartialEq)]
pub enum Piece {
    Answer(String),
    Thought(String),
}

// The splitter's state: the close marker of the span it is inside, text held back because it may start a marker, and whether the answer has started.
pub struct Filter {
    close: Option<&'static str>,
    held: String,
    started: bool,
}

// Returns the length of the longest end of `text` that begins one of the markers, which must wait for more text.
fn partial(text: &str, markers: &[&str]) -> usize {
    (1..=text.len())
        .rev()
        .filter(|&n| text.is_char_boundary(text.len() - n))
        .find(|&n| markers.iter().any(|m| m.len() > n && m.starts_with(&text[text.len() - n..])))
        .unwrap_or(0)
}

impl Filter {
    // Starts inside a span when the prompt opened one it did not close (a template that begins the thinking itself).
    pub fn new(prompt: &str) -> Filter {
        let close = SPANS
            .iter()
            .find(|(o, c)| prompt.rfind(o).is_some_and(|i| prompt.rfind(c).is_none_or(|j| j < i)))
            .map(|(_, c)| *c);
        Filter { close, held: String::new(), started: false }
    }

    // Adds generated text and returns what can be shown now.
    pub fn push(&mut self, text: &str) -> Vec<Piece> {
        self.held.push_str(text);
        let mut out = Vec::new();
        loop {
            let rest = std::mem::take(&mut self.held);
            let Some(close) = self.close else {
                if !self.answer(rest, &mut out) {
                    return out;
                }
                continue;
            };
            let Some(i) = rest.find(close) else {
                let keep = partial(&rest, &[close]);
                self.thought(&rest[..rest.len() - keep], &mut out);
                self.held = rest[rest.len() - keep..].to_string();
                return out;
            };
            self.thought(&rest[..i], &mut out);
            self.held = rest[i + close.len()..].to_string();
            self.close = None;
        }
    }

    // Splits answer text at the first marker; returns true when a marker was found and the rest needs another pass.
    fn answer(&mut self, rest: String, out: &mut Vec<Piece>) -> bool {
        let opens = SPANS.iter().map(|(o, _)| *o);
        let hit = opens
            .chain(DROP)
            .filter_map(|m| rest.find(m).map(|i| (i, m)))
            .min_by_key(|&(i, m)| (i, !DROP.contains(&m)));
        let Some((i, m)) = hit else {
            let markers: Vec<&str> = SPANS.iter().map(|(o, _)| *o).chain(DROP).collect();
            let keep = partial(&rest, &markers);
            self.emit(&rest[..rest.len() - keep], out);
            self.held = rest[rest.len() - keep..].to_string();
            return false;
        };
        self.emit(&rest[..i], out);
        self.close = SPANS.iter().find(|(o, _)| *o == m).map(|(_, c)| *c);
        self.held = rest[i + m.len()..].to_string();
        true
    }

    // Adds answer text, leaving out the blank space a model puts before its answer.
    fn emit(&mut self, text: &str, out: &mut Vec<Piece>) {
        let text = if self.started { text } else { text.trim_start() };
        if !text.is_empty() {
            self.started = true;
            out.push(Piece::Answer(text.to_string()));
        }
    }

    // Adds thinking text.
    fn thought(&mut self, text: &str, out: &mut Vec<Piece>) {
        if !text.is_empty() {
            out.push(Piece::Thought(text.to_string()));
        }
    }

    // Returns the text still held back when generation ends.
    pub fn finish(&mut self) -> Vec<Piece> {
        let rest = std::mem::take(&mut self.held);
        let mut out = Vec::new();
        if self.close.is_some() {
            self.thought(&rest, &mut out);
        } else {
            self.emit(&rest, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Feeds text one character at a time and joins what comes out as (answer, thought).
    fn run(prompt: &str, text: &str) -> (String, String) {
        let mut f = Filter::new(prompt);
        let mut pieces = Vec::new();
        for c in text.chars() {
            pieces.extend(f.push(&c.to_string()));
        }
        pieces.extend(f.finish());
        let (mut a, mut t) = (String::new(), String::new());
        for p in pieces {
            match p {
                Piece::Answer(s) => a.push_str(&s),
                Piece::Thought(s) => t.push_str(&s),
            }
        }
        (a, t)
    }

    // Checks Qwen's thinking, begun by the prompt or by the model, and its empty block when thinking is off.
    #[test]
    fn test_think_qwen() {
        assert_eq!(
            run("<|im_start|>assistant\n<think>\n", "plan</think>\n\nAnswer <b>"),
            ("Answer <b>".into(), "plan".into())
        );
        assert_eq!(run("assistant\n<think>\n\n</think>\n\n", "Hi"), ("Hi".into(), String::new()));
        assert_eq!(run("assistant\n", "<think>x</think> y"), ("y".into(), "x".into()));
    }

    // Checks Gemma 4's thought channel and gpt-oss's analysis and final channels.
    #[test]
    fn test_think_gemma_and_gpt_oss() {
        assert_eq!(
            run("<|turn>model\n", "<|channel>thought\nhmm<channel|>Yes."),
            ("Yes.".into(), "\nhmm".into())
        );
        assert_eq!(
            run("<|turn>model\n<|channel>thought\n<channel|>", "Yes."),
            ("Yes.".into(), String::new())
        );
        let gpt =
            "<|channel|>analysis<|message|>think<|end|><|start|>assistant<|channel|>final<|message|>Done";
        assert_eq!(run("<|start|>assistant", gpt), ("Done".into(), "think".into()));
        assert_eq!(run("", "<|channel|>final<|message|>Done"), ("Done".into(), String::new()));
    }

    // Checks text that only looks like a marker's start is shown once it cannot be one, and multi-byte text is kept whole.
    #[test]
    fn test_think_partial_markers() {
        assert_eq!(run("", "a <thin b é"), ("a <thin b é".into(), String::new()));
        assert_eq!(run("", "tail <"), ("tail <".into(), String::new()));
        assert_eq!(run("<think>", "never closed </thi"), (String::new(), "never closed </thi".into()));
        assert_eq!(partial("ab<|chan", &["<|channel|>final"]), 6);
    }
}
