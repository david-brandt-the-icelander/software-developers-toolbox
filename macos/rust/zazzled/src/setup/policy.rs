// The user's policy for `zazzle setup` (~/.zazzle/policy.toml): extra instruction text and the Claude Code session keys setup owns; only keys the file names are written.
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

// The keys setup may own in Claude Code's settings, at the top level and per model under `modelSettings`.
pub const SESSION_KEYS: &[&str] =
    &["autoCompactWindow", "promptSuggestionEnabled", "idleCompaction", "effortLevel"];
pub const MODEL_KEYS: &[&str] = &["autoCompactWindow"];

// The template written when there is no policy yet: everything commented out, so nothing is owned until the user names it.
pub const TEMPLATE: &str = "\
# zazzle policy: what `zazzle setup --apply` manages. Only keys named here are written;
# `zazzle setup --set KEY=VALUE --apply` edits this file, `zazzle setup --explain` describes it.

[instructions]
# extra = \"Text added to zazzle's managed block in ~/.claude/CLAUDE.md and ~/.codex/AGENTS.md.\"

[claude]
# autoCompactWindow = 160000
# promptSuggestionEnabled = false
# idleCompaction = false
# effortLevel = \"medium\"

# [claude.modelSettings.\"MODEL-ID\"]
# autoCompactWindow = 160000
";

// What the policy asks for: extra instruction text, and the session keys with their values (model keys as `modelSettings.MODEL.KEY`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Policy {
    pub extra: String,
    pub claude: Map<String, Value>,
}

// Returns the policy file's path.
pub fn path(home: &Path) -> PathBuf {
    home.join(".zazzle/policy.toml")
}

// Reads the policy; a missing file is the empty policy.
pub fn load(home: &Path) -> Result<Policy, String> {
    match std::fs::read_to_string(path(home)) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path(home).display())),
        Err(_) => Ok(Policy::default()),
    }
}

// Parses policy text, refusing keys setup does not own.
pub fn parse(text: &str) -> Result<Policy, String> {
    let doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("not valid TOML: {e}"))?;
    let mut p = Policy::default();
    for (k, v) in doc.iter() {
        match (k, v.as_table()) {
            ("instructions", Some(t)) => {
                for (ik, iv) in t.iter() {
                    match (ik, iv.as_str()) {
                        ("extra", Some(s)) => p.extra = s.to_string(),
                        _ => return Err(format!("unknown key instructions.{ik} (known: extra)")),
                    }
                }
            }
            ("claude", Some(t)) => {
                for (ck, cv) in t.iter() {
                    if ck == "modelSettings" {
                        let models = cv.as_table().ok_or("claude.modelSettings must be a table")?;
                        for (m, mv) in models.iter() {
                            let mt =
                                mv.as_table().ok_or("each claude.modelSettings entry must be a table")?;
                            for (mk, v) in mt.iter() {
                                if !MODEL_KEYS.contains(&mk) {
                                    return Err(format!(
                                        "unknown key claude.modelSettings.{m}.{mk} (known: {})",
                                        MODEL_KEYS.join(", ")
                                    ));
                                }
                                p.claude.insert(format!("modelSettings.{m}.{mk}"), value(v)?);
                            }
                        }
                    } else if SESSION_KEYS.contains(&ck) {
                        p.claude.insert(ck.to_string(), value(cv)?);
                    } else {
                        return Err(format!(
                            "unknown key claude.{ck} (known: {}, modelSettings)",
                            SESSION_KEYS.join(", ")
                        ));
                    }
                }
            }
            _ => return Err(format!("unknown section {k} (known: instructions, claude)")),
        }
    }
    Ok(p)
}

// Converts a TOML value to JSON: integers, booleans and strings.
fn value(item: &toml_edit::Item) -> Result<Value, String> {
    let v = item.as_value().ok_or("expected a value")?;
    if let Some(i) = v.as_integer() {
        Ok(Value::from(i))
    } else if let Some(b) = v.as_bool() {
        Ok(Value::from(b))
    } else if let Some(s) = v.as_str() {
        Ok(Value::from(s))
    } else {
        Err(format!("unsupported value {v}"))
    }
}

// Writes the template when the user has no policy yet; returns whether it did.
pub fn ensure(home: &Path) -> Result<bool, String> {
    let p = path(home);
    if p.exists() {
        return Ok(false);
    }
    super::write_file(&p, TEMPLATE)?;
    Ok(true)
}

// Sets keys in the policy text (`KEY=VALUE`, KEY as `claude.KEY`, a bare session key, `instructions.extra`, or `claude.modelSettings.MODEL.KEY`), keeping its comments; an empty VALUE removes the key.
pub fn set(text: &str, sets: &[String]) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut =
        text.parse().map_err(|e| format!("policy is not valid TOML: {e}"))?;
    for s in sets {
        let (k, v) = s.split_once('=').ok_or_else(|| format!("--set {s}: expected KEY=VALUE"))?;
        let k = k.trim();
        let path: Vec<&str> = match k {
            "extra" | "instructions.extra" => vec!["instructions", "extra"],
            _ if SESSION_KEYS.contains(&k) => vec!["claude", k],
            _ => match k.strip_prefix("claude.modelSettings.").and_then(|r| r.rsplit_once('.')) {
                Some((m, key)) => vec!["claude", "modelSettings", m, key],
                None => k.split('.').collect(),
            },
        };
        let (last, tables) = path.split_last().ok_or("empty key")?;
        let mut t = doc.as_table_mut();
        for name in tables {
            let entry = t.entry(name).or_insert(toml_edit::table());
            if let Some(it) = entry.as_inline_table_mut() {
                let owned = std::mem::take(it).into_table();
                *entry = toml_edit::Item::Table(owned);
            }
            t = entry.as_table_mut().ok_or_else(|| format!("--set {s}: {name} is not a table"))?;
            t.set_implicit(true);
        }
        let v = v.trim();
        if v.is_empty() {
            t.remove(last);
            continue;
        }
        let val: toml_edit::Value = if let Ok(i) = v.parse::<i64>() {
            i.into()
        } else if let Ok(b) = v.parse::<bool>() {
            b.into()
        } else {
            v.trim_matches('"').into()
        };
        t.insert(last, toml_edit::value(val));
    }
    let out = doc.to_string();
    parse(&out)?;
    Ok(out)
}

// Describes what the policy controls and what it holds now, for `zazzle setup --explain`.
pub fn explain(p: &Policy, file: &str) -> String {
    let mut out = format!("zazzle setup --explain: what {file} controls.\n\n");
    out.push_str("- instructions.extra: text added inside zazzle's marked block in ~/.claude/CLAUDE.md and ~/.codex/AGENTS.md. Setup never changes text outside the markers.\n");
    out.push_str(
        "- claude.KEY: Claude Code session keys in ~/.claude/settings.json, owned only when named here: ",
    );
    out.push_str(&SESSION_KEYS.join(", "));
    out.push_str("; per model, claude.modelSettings.\"MODEL\".autoCompactWindow. Setup remembers each key's earlier value; removing the key or `--undo` restores it.\n");
    out.push_str("- The hook (unless --no-hook) refuses raw reads, searches, writes and long builds with the exact zazzle command; `# allow-raw` lets one through, logged.\n\n");
    out.push_str("Change: zazzle setup --set KEY=VALUE --apply (an empty VALUE removes the key). Check drift: zazzle setup --check. Undo all: zazzle setup --undo.\n\nNow: ");
    let mut now: Vec<String> = p.claude.iter().map(|(k, v)| format!("{k} = {v}")).collect();
    if !p.extra.is_empty() {
        now.push(format!("instructions.extra ({} characters)", p.extra.chars().count()));
    }
    out.push_str(&if now.is_empty() {
        "nothing owned (generic defaults).".to_string()
    } else {
        now.join("; ")
    });
    out
}

// Returns the JSON path of an owned key in Claude Code's settings: `KEY`, or `modelSettings.MODEL.KEY`.
fn key_path(key: &str) -> Vec<String> {
    match key.strip_prefix("modelSettings.").and_then(|r| r.rsplit_once('.')) {
        Some((m, k)) => vec!["modelSettings".into(), m.into(), k.into()],
        None => vec![key.into()],
    }
}

// Reads an owned key from settings.
pub fn get(settings: &Value, key: &str) -> Option<Value> {
    key_path(key).iter().try_fold(settings, |v, k| v.get(k)).cloned()
}

// Sets an owned key in settings, or removes it (and an emptied model entry) when `value` is None.
fn put(settings: &mut Value, key: &str, value: Option<&Value>) {
    let path = key_path(key);
    let (last, parents) = path.split_last().expect("a key");
    if !settings.is_object() {
        *settings = Value::Object(Map::new());
    }
    let mut at = &mut *settings;
    for p in parents {
        if value.is_none() && at.get(p).is_none() {
            return;
        }
        let o = at.as_object_mut().expect("an object");
        at = o.entry(p.clone()).or_insert_with(|| Value::Object(Map::new()));
        if !at.is_object() {
            *at = Value::Object(Map::new());
        }
    }
    let o = at.as_object_mut().expect("an object");
    match value {
        Some(v) => {
            o.insert(last.clone(), v.clone());
        }
        None => {
            o.remove(last);
        }
    }
    if parents.len() == 2 {
        let models = settings["modelSettings"].as_object_mut().expect("an object");
        if models.get(&parents[1]).is_some_and(|m| m.as_object().is_some_and(Map::is_empty)) {
            models.remove(&parents[1]);
        }
        if models.is_empty() {
            settings.as_object_mut().expect("an object").remove("modelSettings");
        }
    }
}

// Applies the owned keys: sets each wanted key, restores keys owned before but no longer wanted, and returns the lines to report and the record of what each owned key held before setup (null for absent).
pub fn apply_keys(
    settings: &mut Value,
    want: &Map<String, Value>,
    owned: &Map<String, Value>,
) -> (Vec<String>, Map<String, Value>) {
    let mut lines = Vec::new();
    let mut record = Map::new();
    for (k, v) in want {
        let before = owned.get(k).cloned().unwrap_or_else(|| get(settings, k).unwrap_or(Value::Null));
        record.insert(k.clone(), before);
        match get(settings, k) {
            Some(now) if now == *v => lines.push(format!("= {k} = {v}")),
            _ => {
                put(settings, k, Some(v));
                lines.push(format!("~ {k} = {v}"));
            }
        }
    }
    for (k, before) in owned.iter().filter(|(k, _)| !want.contains_key(*k)) {
        put(settings, k, (!before.is_null()).then_some(before));
        lines.push(format!(
            "- {k}: back to {}",
            if before.is_null() { "unset".to_string() } else { before.to_string() }
        ));
    }
    (lines, record)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks the template owns nothing, named keys are read, and keys setup does not own are refused.
    #[test]
    fn test_parse() {
        assert_eq!(parse(TEMPLATE).expect("template"), Policy::default());
        let p = parse("[instructions]\nextra = \"be brief\"\n[claude]\neffortLevel = \"low\"\nidleCompaction = false\nautoCompactWindow = 9\n[claude.modelSettings.\"m-1\"]\nautoCompactWindow = 7\n").expect("policy");
        assert_eq!(p.extra, "be brief");
        assert_eq!(p.claude["effortLevel"], "low");
        assert_eq!(p.claude["idleCompaction"], false);
        assert_eq!(p.claude["modelSettings.m-1.autoCompactWindow"], 7);
        for (bad, why) in [
            ("[claude]\ntheme = \"x\"", "unknown key claude.theme"),
            ("[other]\nx = 1", "unknown section other"),
            ("[instructions]\nmore = \"x\"", "unknown key instructions.more"),
            (
                "[claude.modelSettings.m]\neffortLevel = \"x\"",
                "unknown key claude.modelSettings.m.effortLevel",
            ),
            ("[claude]\nmodelSettings = 1", "must be a table"),
            ("[claude.modelSettings]\nm = 1", "must be a table"),
            ("[claude]\neffortLevel = 1.5", "unsupported value"),
            ("[claude", "not valid TOML"),
        ] {
            assert!(parse(bad).unwrap_err().contains(why), "{bad}");
        }
    }

    // Checks owned keys are set, a second apply changes nothing, and dropping them restores what was there before.
    #[test]
    fn test_apply_keys() {
        let mut settings =
            serde_json::json!({"effortLevel": "high", "theme": "dark", "modelSettings": {"other": {"x": 1}}});
        let want: Map<String, Value> = serde_json::from_value(
            serde_json::json!({"effortLevel": "low", "modelSettings.m.1.autoCompactWindow": 9}),
        )
        .expect("map");
        let (lines, owned) = apply_keys(&mut settings, &want, &Map::new());
        assert_eq!(lines, vec!["~ effortLevel = \"low\"", "~ modelSettings.m.1.autoCompactWindow = 9"]);
        assert_eq!(settings["modelSettings"]["m.1"]["autoCompactWindow"], 9);
        assert_eq!(owned["effortLevel"], "high");
        assert!(owned["modelSettings.m.1.autoCompactWindow"].is_null());
        let snapshot = settings.clone();
        let (again, owned2) = apply_keys(&mut settings, &want, &owned);
        assert!(again.iter().all(|l| l.starts_with("= ")) && settings == snapshot && owned2 == owned);
        let (undo, rest) = apply_keys(&mut settings, &Map::new(), &owned);
        assert_eq!(undo.len(), 2);
        assert!(rest.is_empty());
        assert_eq!(
            settings,
            serde_json::json!({"effortLevel": "high", "theme": "dark", "modelSettings": {"other": {"x": 1}}})
        );
        let mut odd = serde_json::json!(3);
        put(&mut odd, "modelSettings.m.k", None);
        put(&mut odd, "modelSettings.m.k", Some(&Value::from(1)));
        assert_eq!(odd, serde_json::json!({"modelSettings": {"m": {"k": 1}}}));
        put(&mut odd, "modelSettings.m.k", None);
        assert_eq!(odd, serde_json::json!({}));
    }

    // Checks --set edits keep the template's comments, round-trip through parse, remove on an empty value, and refuse unknown keys.
    #[test]
    fn test_set_and_explain() {
        let text = set(
            TEMPLATE,
            &[
                "effortLevel=low".into(),
                "claude.idleCompaction=false".into(),
                "extra=Be brief".into(),
                "claude.modelSettings.m-1.autoCompactWindow=120000".into(),
            ],
        )
        .expect("set");
        assert!(text.contains("# zazzle policy") && text.contains("effortLevel = \"low\""), "{text}");
        let p = parse(&text).expect("parse");
        assert_eq!((p.claude.len(), p.extra.as_str()), (3, "Be brief"));
        assert_eq!(p.claude["modelSettings.m-1.autoCompactWindow"], 120000);
        let removed = parse(&set(&text, &["effortLevel=".into()]).expect("remove")).expect("parse");
        assert!(!removed.claude.contains_key("effortLevel"));
        assert!(set(TEMPLATE, &["claude.theme=dark".into()])
            .unwrap_err()
            .contains("unknown key claude.theme"));
        assert!(set(TEMPLATE, &["nokey".into()]).unwrap_err().contains("expected KEY=VALUE"));
        assert!(set("[x", &[]).is_err());
        assert!(set("claude = 1", &["effortLevel=low".into()]).unwrap_err().contains("not a table"));
        let inline =
            parse(&set("claude = { effortLevel = \"x\" }", &["idleCompaction=true".into()]).expect("inline"))
                .expect("parse");
        assert_eq!(inline.claude.len(), 2);
        let e = explain(&p, "~/.zazzle/policy.toml");
        assert!(
            e.contains("effortLevel = \"low\"") && e.contains("instructions.extra (8 characters)"),
            "{e}"
        );
        assert!(e.len() < 2400, "about 600 tokens at most: {}", e.len());
        assert!(explain(&Policy::default(), "f").contains("nothing owned"));
    }

    // Checks the template is written once and a missing file reads as the empty policy.
    #[test]
    fn test_load_and_ensure() {
        let t = TempDir::new();
        assert_eq!(load(t.path()).expect("missing"), Policy::default());
        assert!(ensure(t.path()).expect("write"));
        assert!(!ensure(t.path()).expect("again"));
        assert_eq!(load(t.path()).expect("template"), Policy::default());
        std::fs::write(path(t.path()), "[x").expect("write");
        assert!(load(t.path()).unwrap_err().contains("policy.toml"));
    }
}
