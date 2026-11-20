// Renders a model's own Jinja chat template the way Hugging Face transformers does, with the thinking and effort switches the catalogue's families read.
use minijinja::{Environment, Error, ErrorKind, Value};
use serde_json::json;

// What the template is given: the system message, the earlier turns (role, text) of a conversation, the new user message, the effort and the special tokens.
pub struct Turn<'a> {
    pub system: Option<&'a str>,
    pub history: &'a [(String, String)],
    pub user: &'a str,
    pub effort: &'a str,
    pub bos: &'a str,
    pub eos: &'a str,
}

// Fails rendering with the template's own message, as transformers' raise_exception does.
fn raise_exception(msg: String) -> Result<Value, Error> {
    Err(Error::new(ErrorKind::InvalidOperation, msg))
}

// Formats the local time with a strftime pattern, for templates that put today's date in the system message.
fn strftime_now(fmt: String) -> Result<Value, Error> {
    let c =
        std::ffi::CString::new(fmt).map_err(|_| Error::new(ErrorKind::InvalidOperation, "NUL in format"))?;
    let mut buf = [0u8; 128];
    // SAFETY: time(NULL) is always valid; localtime_r writes into tm, which we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: as above; t lives for both calls.
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    // SAFETY: t and tm are valid pointers for the call.
    unsafe { libc::localtime_r(&t, &mut tm) };
    // SAFETY: buf is writable for its length, c is NUL-terminated and tm was filled above.
    let n = unsafe { libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c.as_ptr(), &tm) };
    Ok(Value::from(String::from_utf8_lossy(&buf[..n]).into_owned()))
}

// Renders the prompt for one turn: the system message, the earlier turns and the user message, the generation prompt, and `enable_thinking` and `reasoning_effort` from the effort.
pub fn render(template: &str, turn: &Turn) -> Result<String, String> {
    let mut env = Environment::new();
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    env.add_function("raise_exception", raise_exception);
    env.add_function("strftime_now", strftime_now);
    env.add_template("chat", template)
        .map_err(|e| format!("the model's chat template does not parse: {e}"))?;
    let mut messages = Vec::new();
    if let Some(s) = turn.system {
        messages.push(json!({"role": "system", "content": s}));
    }
    for (role, content) in turn.history {
        messages.push(json!({"role": role, "content": content}));
    }
    messages.push(json!({"role": "user", "content": turn.user}));
    let ctx = json!({
        "messages": messages,
        "add_generation_prompt": true,
        "enable_thinking": turn.effort != "low",
        "reasoning_effort": turn.effort,
        "bos_token": turn.bos,
        "eos_token": turn.eos,
    });
    let tmpl = env.get_template("chat").map_err(|e| e.to_string())?;
    tmpl.render(Value::from_serialize(&ctx)).map_err(|e| format!("rendering the model's chat template: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Returns a turn with the given effort.
    fn turn(effort: &str) -> Turn<'_> {
        Turn { system: Some("Be brief."), history: &[], user: "Hi?", effort, bos: "<s>", eos: "</s>" }
    }

    // A Qwen-style template: ChatML with an empty thinking block when thinking is off.
    const QWEN: &str = "{%- for m in messages %}<|im_start|>{{ m.role }}\n{{ m.content | trim }}<|im_end|>\n{% endfor %}\
{%- if add_generation_prompt %}<|im_start|>assistant\n{% if enable_thinking is defined and enable_thinking is true %}<think>\n{% else %}<think>\n\n</think>\n\n{% endif %}{% endif %}";

    // Checks the thinking switch follows the effort, and the messages and generation prompt come out in order.
    #[test]
    fn test_template_thinking() {
        let on = render(QWEN, &turn("high")).expect("high");
        assert_eq!(on, "<|im_start|>system\nBe brief.<|im_end|>\n<|im_start|>user\nHi?<|im_end|>\n<|im_start|>assistant\n<think>\n");
        assert!(render(QWEN, &turn("low")).expect("low").ends_with("<think>\n\n</think>\n\n"));
        let history = [("user".to_string(), "One?".to_string()), ("assistant".to_string(), "1".to_string())];
        let chat = render(QWEN, &Turn { system: None, history: &history, ..turn("low") }).expect("chat");
        assert!(
            chat.starts_with(
                "<|im_start|>user\nOne?<|im_end|>\n<|im_start|>assistant\n1<|im_end|>\n<|im_start|>user\nHi?"
            ),
            "{chat}"
        );
    }

    // Checks the Python methods, tojson, raise_exception, strftime_now and the effort name reach the template.
    #[test]
    fn test_template_helpers() {
        let t = "{{ bos_token }}{% if messages[0].content.startswith('Be') %}{{ reasoning_effort | upper }}{% endif %}\
{{ messages[1] | tojson }}{{ strftime_now('%Y') | length }}";
        assert_eq!(
            render(t, &turn("medium")).expect("ok"),
            "<s>MEDIUM{\"content\":\"Hi?\",\"role\":\"user\"}4"
        );
        let bad = "{{ raise_exception('no tools here') }}";
        assert!(render(bad, &turn("low")).expect_err("raise").contains("no tools here"));
        assert!(render("{% if %}", &turn("low")).expect_err("parse").contains("does not parse"));
    }
}
