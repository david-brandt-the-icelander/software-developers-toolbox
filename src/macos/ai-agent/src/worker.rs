// `ai-agent worker`: keeps one model loaded and answers prompts sent as JSON lines on standard input, one JSON line each on standard output, with a conversation's earlier turns when given, so daemon keeps the model loaded for chats and agents' calls.
use crate::{engine, think};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

// One prompt as the worker receives it, with the earlier turns of a conversation.
pub struct Request<'a> {
    pub system: Option<&'a str>,
    pub history: &'a [(String, String)],
    pub user: &'a str,
    pub effort: &'a str,
    pub max_tokens: usize,
    pub grammar: Option<&'a str>,
    pub temp: f32,
}

// Something that answers one prompt: the answer text without thinking, and how it ended (tokens generated, whether the limit cut it short, the prompt's tokens and how many the kept context reused).
pub trait Gen {
    fn ask(&self, r: &Request) -> Result<(String, engine::Done), String>;
}

// A loaded model answering prompts.
pub struct Model(pub engine::Loaded);

impl Gen for Model {
    // Generates an answer, keeping only the text after the model's thinking.
    fn ask(&self, r: &Request) -> Result<(String, engine::Done), String> {
        let ask = engine::Ask {
            path: std::path::Path::new(""),
            system: r.system,
            history: r.history,
            user: r.user,
            effort: r.effort,
            max_tokens: r.max_tokens,
            grammar: r.grammar,
            temp: r.temp,
        };
        let (mut filter, mut answer) = (think::Filter::new(""), String::new());
        let mut keep = |pieces: Vec<think::Piece>| {
            for p in pieces {
                if let think::Piece::Answer(s) = p {
                    answer.push_str(&s);
                }
            }
        };
        let done = self.0.generate(&ask, &mut |ev| match ev {
            engine::Event::Prompt(p) => filter = think::Filter::new(p),
            engine::Event::Text(t) => keep(filter.push(t)),
        })?;
        keep(filter.finish());
        Ok((answer, done))
    }
}

// The defaults a request may override: effort, token limit and temperature.
pub struct Defaults {
    pub effort: String,
    pub max_tokens: usize,
    pub temp: f32,
}

// Reads `[{"role", "content"}..]` as the earlier turns, skipping entries without both.
fn history(v: &Value) -> Vec<(String, String)> {
    let items = v.as_array().map(Vec::as_slice).unwrap_or_default();
    items
        .iter()
        .filter_map(|m| Some((m["role"].as_str()?.to_string(), m["content"].as_str()?.to_string())))
        .collect()
}

// Answers one request line `{"user", "history"?, "system"?, "effort"?, "max_tokens"?, "temp"?, "grammar"?}` with `{"text", "tokens", "truncated", "prompt_tokens", "cached_tokens"}`, or `{"error"}`.
pub fn answer(gen: &dyn Gen, line: &str, d: &Defaults) -> Value {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("not a JSON request: {e}")}),
    };
    let Some(user) = req["user"].as_str() else { return json!({"error": "the request has no `user` text"}) };
    let effort =
        match req["effort"].as_str().map(crate::store::effort).unwrap_or_else(|| Ok(d.effort.clone())) {
            Ok(e) => e,
            Err(e) => return json!({"error": e}),
        };
    let past = history(&req["history"]);
    let r = Request {
        system: req["system"].as_str(),
        history: &past,
        user,
        effort: &effort,
        max_tokens: req["max_tokens"].as_u64().map_or(d.max_tokens, |n| n as usize),
        grammar: req["grammar"].as_str(),
        temp: req["temp"].as_f64().map_or(d.temp, |t| t as f32),
    };
    match gen.ask(&r) {
        Ok((text, d)) => json!({
            "text": text,
            "tokens": d.tokens,
            "truncated": d.truncated,
            "prompt_tokens": d.prompt,
            "cached_tokens": d.reused,
        }),
        Err(e) => json!({"error": e}),
    }
}

// Writes a ready line naming the model, then answers each request line until standard input closes.
pub fn serve(
    gen: &dyn Gen,
    name: &str,
    d: &Defaults,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<(), String> {
    let io = |e: std::io::Error| format!("worker stream: {e}");
    writeln!(output, "{}", json!({"ready": name, "effort": d.effort}))
        .and_then(|()| output.flush())
        .map_err(io)?;
    for line in input.lines() {
        let line = line.map_err(io)?;
        if line.trim().is_empty() {
            continue;
        }
        writeln!(output, "{}", answer(gen, &line, d)).and_then(|()| output.flush()).map_err(io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Answers with what it was given, or fails on `fail`.
    struct Echo;

    impl Gen for Echo {
        // Echoes the request.
        fn ask(&self, r: &Request) -> Result<(String, engine::Done), String> {
            if r.user == "fail" {
                return Err("boom".into());
            }
            let text = format!(
                "{}{}|{}|{}|{}|{}|{}",
                r.history.iter().map(|(a, b)| format!("{a}:{b};")).collect::<String>(),
                r.system.unwrap_or("-"),
                r.user,
                r.effort,
                r.max_tokens,
                r.temp,
                r.grammar.unwrap_or("-")
            );
            let done = engine::Done { tokens: 3, truncated: r.max_tokens < 10, prompt: 20, reused: 12 };
            Ok((text, done))
        }
    }

    // Checks the ready line, an answer per request, defaults and overrides, and errors as JSON lines.
    #[test]
    fn test_worker_serve() {
        let input = "{\"user\":\"q\"}\n\n{\"user\":\"q\",\"system\":\"s\",\"effort\":\"high\",\"max_tokens\":5,\"temp\":0,\"grammar\":\"g\"}\n{\"user\":\"fail\"}\nnot json\n{}\n{\"user\":\"q\",\"effort\":\"max\"}\n{\"user\":\"q\",\"history\":[{\"role\":\"user\",\"content\":\"a\"},{\"role\":\"assistant\"}]}\n";
        let mut out = Vec::new();
        let d = Defaults { effort: "low".into(), max_tokens: 100, temp: 0.5 };
        serve(&Echo, "m", &d, &mut input.as_bytes(), &mut out).expect("serve");
        let lines: Vec<Value> = String::from_utf8(out)
            .expect("utf8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("json"))
            .collect();
        assert_eq!(lines[0], json!({"ready": "m", "effort": "low"}));
        assert_eq!(
            lines[1],
            json!({"text": "-|q|low|100|0.5|-", "tokens": 3, "truncated": false, "prompt_tokens": 20, "cached_tokens": 12})
        );
        assert_eq!(
            lines[2],
            json!({"text": "s|q|high|5|0|g", "tokens": 3, "truncated": true, "prompt_tokens": 20, "cached_tokens": 12})
        );
        assert_eq!(lines[3], json!({"error": "boom"}));
        assert!(lines[4]["error"].as_str().expect("e").starts_with("not a JSON request"));
        assert_eq!(lines[5], json!({"error": "the request has no `user` text"}));
        assert!(lines[6]["error"].as_str().expect("e").contains("max"));
        assert_eq!(lines[7]["text"], "user:a;-|q|low|100|0.5|-");
    }
}
