// Per-tool proxy settings from a JSON file: lossless, budget and skip for each tool (exact name or * pattern) and for resource reads.
use serde_json::{Map, Value};

const SECTIONS: [&str; 2] = ["tools", "resources"];
const KEYS: [&str; 3] = ["lossless", "budget", "skip"];

// Settings for one tool or for resource reads; unset fields fall back to the command line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rule {
    pub lossy: Option<bool>,
    pub budget: Option<usize>,
    pub skip: bool,
}

// The rules of a config file: per tool name or pattern, and for resources/read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub tools: Vec<(String, Rule)>,
    pub resources: Option<Rule>,
}

// Returns a JSON value as an object, or says where it should have been one.
fn object<'a>(at: &str, v: &'a Value) -> Result<&'a Map<String, Value>, String> {
    v.as_object().ok_or_else(|| format!("{at} must be an object"))
}

// Reads one rule, refusing unknown settings and wrong types so a typo cannot pass silently.
fn rule(at: &str, v: &Value) -> Result<Rule, String> {
    let obj = object(at, v)?;
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("{at}: unknown setting '{k}'; use lossless, budget or skip"));
    }
    let flag = |key: &str| match obj.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{at}.{key} must be true or false")),
    };
    let budget = match obj.get("budget") {
        None => None,
        Some(n) => Some(
            n.as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{at}.budget must be a number of at least 1"))? as usize,
        ),
    };
    Ok(Rule { lossy: flag("lossless")?.map(|l| !l), budget, skip: flag("skip")?.unwrap_or(false) })
}

// Parses a config file's text.
pub fn parse(text: &str) -> Result<Config, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let obj = object("the config", &v)?;
    if let Some(k) = obj.keys().find(|k| !SECTIONS.contains(&k.as_str())) {
        return Err(format!("unknown section '{k}'; use tools or resources"));
    }
    let tools = match obj.get("tools") {
        None => Vec::new(),
        Some(t) => object("tools", t)?
            .iter()
            .map(|(name, r)| Ok((name.clone(), rule(&format!("tools.{name}"), r)?)))
            .collect::<Result<_, String>>()?,
    };
    let resources = obj.get("resources").map(|r| rule("resources", r)).transpose()?;
    Ok(Config { tools, resources })
}

// Reads and parses a config file.
pub fn load(path: &str) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("--config {path}: {e}"))?;
    parse(&text).map_err(|e| format!("--config {path}: {e}"))
}

// True when a name matches a pattern in which `*` stands for any run of characters.
fn matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, .., last] = parts.as_slice() else { return pattern == name };
    if name.len() < first.len() + last.len() || !name.starts_with(first) || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        let Some(i) = rest.find(mid) else { return false };
        rest = &rest[i + mid.len()..];
    }
    true
}

impl Config {
    // Returns the rule for a tool: its exact name first, else the longest pattern that matches.
    pub fn tool(&self, name: &str) -> Option<&Rule> {
        let exact = self.tools.iter().find(|(p, _)| p == name);
        let pattern = || {
            self.tools
                .iter()
                .filter(|(p, _)| p.contains('*') && matches(p, name))
                .max_by_key(|(p, _)| p.len())
        };
        exact.or_else(pattern).map(|(_, r)| r)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks a full config parses into rules, with lossless turned into its lossy setting.
    #[test]
    fn test_parse() {
        let c = parse(r#"{"tools": {"read_file": {"lossless": true, "budget": 30000}, "github_*": {"budget": 2000}, "secret": {"skip": true}}, "resources": {"lossless": false}}"#).expect("config");
        assert_eq!(c.tool("read_file"), Some(&Rule { lossy: Some(false), budget: Some(30000), skip: false }));
        assert_eq!(c.tool("secret"), Some(&Rule { skip: true, ..Rule::default() }));
        assert_eq!(c.resources, Some(Rule { lossy: Some(true), ..Rule::default() }));
        assert_eq!(parse("{}").expect("empty"), Config::default());
    }

    // Checks every kind of mistake is refused with where it is.
    #[test]
    fn test_parse_errors() {
        let err = |t: &str| parse(t).unwrap_err();
        assert!(err("nope").starts_with("not JSON: "));
        assert_eq!(err("[]"), "the config must be an object");
        assert_eq!(err(r#"{"tool": {}}"#), "unknown section 'tool'; use tools or resources");
        assert_eq!(err(r#"{"tools": []}"#), "tools must be an object");
        assert_eq!(err(r#"{"tools": {"a": 1}}"#), "tools.a must be an object");
        assert_eq!(
            err(r#"{"tools": {"a": {"lossy": true}}}"#),
            "tools.a: unknown setting 'lossy'; use lossless, budget or skip"
        );
        assert_eq!(err(r#"{"tools": {"a": {"skip": "yes"}}}"#), "tools.a.skip must be true or false");
        assert_eq!(
            err(r#"{"tools": {"a": {"budget": 0}}}"#),
            "tools.a.budget must be a number of at least 1"
        );
        assert_eq!(
            err(r#"{"resources": {"budget": "x"}}"#),
            "resources.budget must be a number of at least 1"
        );
    }

    // Checks exact names win, then the longest matching pattern, and patterns match anywhere a star allows.
    #[test]
    fn test_tool_lookup() {
        let c = parse(r#"{"tools": {"*": {"budget": 1}, "github_*": {"budget": 2}, "github_*_file": {"budget": 3}, "github_get_file": {"budget": 4}}}"#).expect("config");
        let budget = |n: &str| c.tool(n).and_then(|r| r.budget);
        assert_eq!(budget("github_get_file"), Some(4));
        assert_eq!(budget("github_put_file"), Some(3));
        assert_eq!(budget("github_search"), Some(2));
        assert_eq!(budget("other"), Some(1));
        assert_eq!(Config::default().tool("x"), None);
        assert!(matches("a*b*c", "aXbYc") && !matches("a*b*c", "aXcYb") && !matches("ab*ba", "aba"));
        assert!(matches("exact", "exact") && !matches("exact", "exactly"));
    }

    // Checks a config file is read from disk, and a missing or broken one names the file.
    #[test]
    fn test_load() {
        let t = crate::testutil::TempDir::new();
        let good = t.write("p.json", br#"{"resources": {"skip": true}}"#);
        assert!(load(&good.to_string_lossy()).expect("load").resources.is_some_and(|r| r.skip));
        let bad = t.write("bad.json", b"{");
        assert!(load(&bad.to_string_lossy()).unwrap_err().contains("bad.json: not JSON"));
        assert!(load("/nonexistent/p.json").unwrap_err().starts_with("--config /nonexistent/p.json: "));
    }
}
