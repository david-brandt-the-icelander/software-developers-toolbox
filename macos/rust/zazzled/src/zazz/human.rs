// Human runs: `zazz run NAME ARGS` and installed shims parse argv with the manifest's cli spec and print plain output.
use super::call::{self, Streams};
use super::pool::{from_sdk, to_sdk};
use crate::cmd::{Ctx, Output};
use serde_json::{json, Value};
use std::os::fd::AsRawFd;
use zazz::cliargs::{Parsed, Spec};

// A one-line error for a human, with the exit status to use.
fn fail(name: &str, e: &Value, status: u8) -> Output {
    let text = format!(
        "{name}: {}: {}",
        e["code"].as_str().unwrap_or("failed"),
        e["message"].as_str().unwrap_or("")
    );
    Output { text, status, idable: false, raw: false }
}

// Turns argv into the script's input: parsed by the cli spec, or {"argv": [...]} without one.
fn input_for(
    name: &str,
    manifest: &Value,
    argv: &[String],
    cwd: &str,
) -> Result<Result<Value, String>, String> {
    let Some(cli) = manifest.get("cli") else { return Ok(Ok(json!({"argv": argv}))) };
    let spec = Spec::from_json(name, &to_sdk(cli))?;
    match spec.parse(argv, cwd) {
        Ok(Parsed::Input(j)) => Ok(Ok(from_sdk(&j))),
        Ok(Parsed::Help(text)) => Ok(Err(text)),
        Err(e) => Err(e),
    }
}

// True when a descriptor is a terminal.
fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

// Renders the returned output for a human: raw JSON with --json, otherwise strings, tables or indented JSON.
fn render(output: &Value, json_out: bool, tty: bool) -> String {
    if json_out {
        return if output.is_null() {
            String::new()
        } else {
            format!("{}\n", serde_json::to_string_pretty(output).unwrap_or_default())
        };
    }
    zazz::render::render(&to_sdk(output), tty)
}

// Takes zazz's own --json flag out of argv, unless the script's cli spec defines --json itself.
fn split_json(manifest: &Value, args: &[String]) -> (bool, Vec<String>) {
    let own = manifest["cli"]["args"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x["long"] == "json" || x["name"] == "json"));
    if own {
        return (false, args.to_vec());
    }
    (args.iter().any(|a| a == "--json"), args.iter().filter(|a| *a != "--json").cloned().collect())
}

// Runs a registered script for a human: `what` is an installed name or an id.
pub fn run(ctx: &Ctx, what: &str, args: &[String]) -> Output {
    let z = &ctx.engine.zazz;
    let found = z.registry().and_then(|r| r.resolve(what).map(|hex| (r.read_meta(&hex), hex)));
    let (meta, hex) = match found {
        Ok((Some(m), hex)) => (m, hex),
        Ok((None, _)) | Err(_) => {
            return fail(
                what,
                &json!({"code": "not_found", "message": "no such installed script; see `zazz list`"}),
                127,
            )
        }
    };
    let name = meta["name"].as_str().unwrap_or(what).to_string();
    let (json_out, argv) = split_json(&meta["manifest"], args);
    let input = match input_for(&name, &meta["manifest"], &argv, &ctx.cwd.to_string_lossy()) {
        Ok(Ok(v)) => v,
        Ok(Err(help)) => return Output::raw(help + "\n", 0),
        Err(e) => return Output { text: e, status: 2, idable: false, raw: false },
    };
    let (streams, tty) = match ctx.caller.borrow_mut().take() {
        Some(c) => {
            let tty = is_tty(c.stdio[1].as_raw_fd());
            (Streams::Caller { stdio: c.stdio, conn: c.conn }, tty)
        }
        None => (Streams::Capture { stdin: ctx.read_stdin().unwrap_or_default() }, false),
    };
    let msg = json!({"t": "json-command", "mode": "human", "input": input});
    let called = match call::execute(z, &hex, &ctx.cwd, msg, streams) {
        Ok(c) => c,
        Err(e) => return fail(&name, &e, 1),
    };
    let exit = called.result["exit"].as_i64().unwrap_or(0).clamp(0, 255) as u8;
    if called.result["ok"] != true {
        return fail(&name, &called.result["error"], exit.max(1));
    }
    let text = called.stdout + &render(&called.result["output"], json_out, tty);
    Output::raw(text, exit)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks input building with and without a cli spec, help and parse errors.
    #[test]
    fn test_input_for() {
        let argv = vec!["a.txt".to_string()];
        assert_eq!(
            input_for("s", &json!({}), &argv, "/w").expect("ok").expect("input"),
            json!({"argv": ["a.txt"]})
        );
        let m = json!({"cli": {"args": [{"name": "files", "positional": true, "type": "path", "multiple": true}]}});
        assert_eq!(
            input_for("s", &m, &argv, "/w").expect("ok").expect("input"),
            json!({"files": ["/w/a.txt"]})
        );
        assert!(input_for("s", &m, &["--help".into()], "/w")
            .expect("ok")
            .unwrap_err()
            .starts_with("usage: s"));
        assert!(input_for("s", &m, &["--bogus".into()], "/w")
            .unwrap_err()
            .contains("unknown option --bogus"));
        assert!(input_for("s", &json!({"cli": {"stdin": "x"}}), &argv, "/w").is_err());
    }

    // Checks --json is zazz's unless the script defines it.
    #[test]
    fn test_split_json() {
        let a = vec!["x".to_string(), "--json".to_string()];
        assert_eq!(split_json(&json!({}), &a), (true, vec!["x".to_string()]));
        let own = json!({"cli": {"args": [{"name": "json", "type": "bool"}]}});
        assert_eq!(split_json(&own, &a), (false, a.clone()));
        let long = json!({"cli": {"args": [{"name": "other"}, {"name": "j", "long": "json"}]}});
        assert_eq!(split_json(&long, &a), (false, a.clone()));
        assert_eq!(split_json(&json!({}), &a[..1]), (false, vec!["x".to_string()]));
    }

    // Checks rendering for humans and --json.
    #[test]
    fn test_render() {
        assert_eq!(render(&json!({"a": 1}), true, false), "{\n  \"a\": 1\n}\n");
        assert_eq!(render(&Value::Null, true, false), "");
        assert_eq!(render(&json!("hi"), false, false), "hi\n");
        assert!(!is_tty(-1));
        let f = fail("s", &json!({"code": "bad_input", "message": "m"}), 2);
        assert_eq!((f.text.as_str(), f.status, f.raw), ("s: bad_input: m", 2, false));
    }
}
