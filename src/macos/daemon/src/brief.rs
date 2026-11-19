// `brief INTENT`: the guidance an agent needs before a task. Paths and purposes narrow what applies before any inference; every applying rule section is included, the local model only ranks the optional skills, and what is sent is recorded on the session's shared rule ledger, cut at section boundaries to the budget.
use crate::rules::{canonical, file_key, range_key, Ledger};
use ai_agent_rules::{body, purpose, Found, Kind};
use regex::Regex;
use std::path::{Path, PathBuf};

const MENU_CHARS: usize = 600;
pub const DEFAULT_BUDGET: usize = 8000;

// The MCP tool for brief.
pub(crate) const TOOLS: &[crate::mcp::Tool] = &[crate::mcp::Tool {
    name: "zazzle_brief",
    description: "Before a task: every rule section that applies to the paths (narrowed to the purposes), then the skills the local model ranks as needed for your intent, verbatim and cut only at section boundaries; what this session already has and what was left out are named in one line each.",
    schema,
    map: args,
}];

pub const USAGE: &str = "usage: brief INTENT.. [--path PATH].. [--purpose NAME].. [--budget TOKENS]";

// Schema for zazzle_brief.
fn schema() -> serde_json::Value {
    crate::mcp::object(
        serde_json::json!({
            "intent": {"type": "string", "description": "What you are about to do, in a sentence"},
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Files or folders the work touches (default: the working directory)"},
            "purpose": {"type": "array", "items": {"type": "string"}, "description": "Purposes whose marked sections apply, e.g. review (default: the session's last get-rules purposes)"},
            "budget": {"type": "integer", "description": "Token budget (default 8000); rule sections are always sent"}
        }),
        &["intent"],
    )
}

// Turns zazzle_brief arguments into a command line.
fn args(v: &serde_json::Value) -> Result<Vec<String>, String> {
    let mut a = vec!["brief".to_string(), crate::mcp::need_str(v, "intent")?];
    for p in crate::mcp::get_list(v, "paths")? {
        a.extend(["--path".to_string(), p]);
    }
    for p in crate::mcp::get_list(v, "purpose")? {
        a.extend(["--purpose".to_string(), p]);
    }
    crate::mcp::push_num(&mut a, v, "budget", "--budget")?;
    Ok(a)
}

// One piece an agent may need: a rule file's lines under one heading (inside the purpose range that applies), or a whole skill.
#[derive(Debug, PartialEq)]
struct Section {
    kind: Kind,
    path: PathBuf,
    lines: (usize, usize),
    within: Option<(usize, usize)>,
    heading: String,
    text: String,
    menu: String,
}

impl Section {
    // Tells whether the section is mandatory (a rule or import) rather than optional guidance for the model to rank (a skill).
    fn mandatory(&self) -> bool {
        self.kind != Kind::Skill
    }
}

// Splits numbered lines of a rule or import file at Markdown headings (outside code fences, purpose markers left out) into sections of verbatim lines.
fn split(f: &Found, lines: &[(usize, &str)], within: Option<(usize, usize)>) -> Vec<Section> {
    let lines: Vec<(usize, &str)> = lines.iter().copied().filter(|(_, l)| !purpose::is_marker(l)).collect();
    let (mut starts, mut fence) = (vec![0], false);
    for (i, (_, l)) in lines.iter().enumerate() {
        fence ^= l.trim_start().starts_with("```");
        if !fence && i > 0 && l.starts_with('#') {
            starts.push(i);
        }
    }
    starts.push(lines.len());
    starts
        .windows(2)
        .filter(|w| w[0] < w[1])
        .filter_map(|w| {
            let part: Vec<&str> = lines[w[0]..w[1]].iter().map(|(_, l)| *l).collect();
            let text = part.join("\n");
            let heading = match part[0].starts_with('#') {
                true => part[0].trim_start_matches('#').trim().to_string(),
                false => String::new(),
            };
            (!text.trim().is_empty()).then(|| Section {
                kind: f.kind,
                path: f.path.clone(),
                lines: (lines[w[0]].0, lines[w[1] - 1].0),
                within,
                heading,
                menu: String::new(),
                text,
            })
        })
        .collect()
}

// Turns the found files into sections: rules and imports by heading inside the ranges that apply, in the order found, then each skill whole, offered to the model by its description.
fn sections(found: &[Found]) -> Vec<Section> {
    let mut out = Vec::new();
    for f in found {
        let text = f.text.as_deref().unwrap_or_default();
        let numbered: Vec<(usize, &str)> = text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
        if f.kind != Kind::Skill {
            if f.lines.is_empty() {
                out.extend(split(f, &numbered, None));
            }
            for &(a, b) in &f.lines {
                let part = &numbered[(a - 1).min(numbered.len())..b.min(numbered.len())];
                out.extend(split(f, part, Some((a, b))));
            }
            continue;
        }
        let desc = body(f, false).lines().skip(1).collect::<Vec<_>>().join(" ");
        out.push(Section {
            kind: f.kind,
            path: f.path.clone(),
            lines: (1, numbered.len()),
            within: None,
            heading: String::new(),
            text: text.to_string(),
            menu: desc.chars().take(MENU_CHARS).collect(),
        });
    }
    out.sort_by_key(|x| !x.mandatory());
    out
}

// Shows a path relative to the working directory when it is under it.
fn shown(p: &Path, cwd: &Path) -> String {
    p.strip_prefix(cwd).unwrap_or(p).display().to_string()
}

// Names a section in one line: kind, path, line range (for rules) and heading.
fn title(s: &Section, cwd: &Path) -> String {
    let h = if s.heading.is_empty() { String::new() } else { format!(" § {}", s.heading) };
    let r = if s.mandatory() { format!(":{}-{}", s.lines.0, s.lines.1) } else { String::new() };
    format!("{} {}{r}{h}", s.kind.name(), shown(&s.path, cwd))
}

// Formats one section verbatim under its header.
fn block(s: &Section, cwd: &Path) -> String {
    let r = if s.mandatory() { format!(":{}-{}", s.lines.0, s.lines.1) } else { String::new() };
    format!("==> {} {}{r} <==\n{}\n\n", s.kind.name(), shown(&s.path, cwd), s.text.trim_end())
}

// The ledger keys of a section: its own range (a skill: its whole file), and the purpose range or whole file a rule section was cut from.
fn keys(s: &Section, text: &str) -> (String, Option<String>) {
    let canon = canonical(&s.path);
    if !s.mandatory() {
        return (file_key(&canon, text), None);
    }
    let outer = match s.within {
        Some(r) => range_key(&canon, text, r),
        None => file_key(&canon, text),
    };
    (range_key(&canon, text, s.lines), Some(outer))
}

// Tells whether the session already has a section: its whole file, the range it was cut from, or the section itself.
fn delivered(s: &Section, text: &str, l: &Ledger) -> bool {
    let (own, outer) = keys(s, text);
    (l.has)(&file_key(&canonical(&s.path), text)) || outer.is_some_and(|k| (l.has)(&k)) || (l.has)(&own)
}

// Records the sections sent: each one's own key, and for a rule section the range or file it came from, which is then complete, since every rule section not already delivered is always sent.
fn record(all: &[Section], texts: &[&str], sent: &[usize], l: &Ledger) {
    for &i in sent {
        let (own, outer) = keys(&all[i], texts[i]);
        std::iter::once(own).chain(outer).for_each(|k| (l.mark)(&k));
    }
}

// Builds what the local model reads: the numbered optional sections with their descriptions.
fn menu(optional: &[&Section], cwd: &Path) -> String {
    optional
        .iter()
        .enumerate()
        .map(|(i, s)| format!("[{}] {}\n{}\n\n", i + 1, title(s, cwd), s.menu.trim()))
        .collect()
}

// A GBNF grammar that makes the model rank every optional section in turn, `1:h 2:n ..` (h needed, m may help, n not), so a small model cannot stop early or ramble.
fn verdicts(n: usize) -> String {
    let items: Vec<String> = (1..=n).map(|k| format!("\"{k}:\" v")).collect();
    format!("root ::= {}\nv ::= \"h\" | \"m\" | \"n\"", items.join(" \" \" "))
}

// Reads the model's ranking: the needed sections, then those that may help, each once and in range (a `y` counts as needed).
fn ranked(answer: &str, n: usize) -> Vec<usize> {
    static RANK: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RANK.get_or_init(|| Regex::new(r"(\d+):([hmy])").expect("valid regex"));
    let (mut high, mut maybe): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    for c in re.captures_iter(answer) {
        let k = c[1].parse::<usize>().unwrap_or(0);
        if !(1..=n).contains(&k) || high.contains(&(k - 1)) || maybe.contains(&(k - 1)) {
            continue;
        }
        match &c[2] {
            "m" => maybe.push(k - 1),
            _ => high.push(k - 1),
        }
    }
    high.extend(maybe);
    high
}

// The question put to the local model.
fn instruction(intent: &str) -> String {
    format!(
        "An agent is about to do this task: {intent}\n\nThe rules that apply are already given to it. Below are numbered skills it could also read. For each one write its number, a colon, then h if the agent needs it for this task, m if it may help, or n if it is not about this task."
    )
}

// What brief needs from where it runs: the working directory, home, the session's purposes and ledger, the budget and a way to ask the model.
pub struct Env<'a> {
    pub cwd: &'a Path,
    pub home: Option<&'a Path>,
    pub purposes: Vec<String>,
    pub ledger: Option<&'a Ledger<'a>>,
    pub budget: usize,
    pub ask: &'a dyn Fn(&str, &str, &str) -> Result<String, String>,
}

// The optional sections to send in rank order, those over the budget and those not needed.
type Ranked = (Vec<usize>, Vec<usize>, Vec<usize>);

// Asks the model to rank the optional sections not yet delivered; returns those to send in rank order, those over the budget and those not needed, or why it could not ask.
fn rank(intent: &str, all: &[Section], optional: &[usize], used: usize, env: &Env) -> Result<Ranked, String> {
    if optional.is_empty() {
        return Ok(Default::default());
    }
    let refs: Vec<&Section> = optional.iter().map(|&i| &all[i]).collect();
    let input = format!("{}The task again: {intent}\nVerdicts:", menu(&refs, env.cwd));
    let order = ranked(&(env.ask)(&instruction(intent), &input, &verdicts(optional.len()))?, optional.len());
    let (mut take, mut over, mut used) = (Vec::new(), Vec::new(), used);
    for &k in &order {
        let cost = crate::tokens::estimate(&block(&all[optional[k]], env.cwd));
        if used + cost <= env.budget {
            used += cost;
            take.push(optional[k]);
        } else {
            over.push(optional[k]);
        }
    }
    let rest = optional.iter().enumerate().filter(|(k, _)| !order.contains(k)).map(|(_, &i)| i).collect();
    Ok((take, over, rest))
}

// Builds a brief from found files: the mandatory sections always, optional ones ranked by the model and added whole while they fit the budget; records what was sent.
fn brief(intent: &str, scope: &str, found: &[Found], env: &Env) -> String {
    let all = sections(found);
    let texts: Vec<&str> = all
        .iter()
        .map(|s| found.iter().find(|f| f.path == s.path).and_then(|f| f.text.as_deref()).unwrap_or_default())
        .collect();
    let mut head = format!("brief for: {intent}\nscope: {scope}");
    if !env.purposes.is_empty() {
        head.push_str(&format!("; purposes: {}", env.purposes.join(", ")));
    }
    if all.is_empty() {
        return format!("{head}\nno rule or skill files apply here");
    }
    let had: Vec<bool> =
        (0..all.len()).map(|i| env.ledger.is_some_and(|l| delivered(&all[i], texts[i], l))).collect();
    let already: Vec<usize> = (0..all.len()).filter(|&i| had[i]).collect();
    let rules: Vec<usize> = (0..all.len()).filter(|&i| !had[i] && all[i].mandatory()).collect();
    let optional: Vec<usize> = (0..all.len()).filter(|&i| !had[i] && !all[i].mandatory()).collect();
    let rule_tokens: usize = rules.iter().map(|&i| crate::tokens::estimate(&block(&all[i], env.cwd))).sum();
    let (skills, over, left, note) = match rank(intent, &all, &optional, rule_tokens, env) {
        Ok((t, o, r)) => (t, o, r, String::new()),
        Err(e) => (Vec::new(), Vec::new(), optional.clone(), format!("skills not ranked: {e}\n")),
    };
    let mut out = format!(
        "{head}\n{note}{} rule sections that apply (always sent); {} of {} skills, ranked by the local model\n\n",
        rules.len(),
        skills.len(),
        optional.len()
    );
    let sent: Vec<usize> = rules.iter().chain(&skills).copied().collect();
    sent.iter().for_each(|&i| out.push_str(&block(&all[i], env.cwd)));
    if let Some(l) = env.ledger {
        record(&all, &texts, &sent, l);
    }
    let names = |v: &[usize]| v.iter().map(|&i| title(&all[i], env.cwd)).collect::<Vec<_>>().join("; ");
    if rule_tokens > env.budget {
        out.push_str(&format!(
            "-- the rule sections alone are ~{rule_tokens} tokens, over the budget of {}; they are sent whole\n",
            env.budget
        ));
    }
    for (v, what) in [
        (&already, "already delivered this session, still in force"),
        (&over, "needed but over the budget, left out whole (raise --budget or read them)"),
        (&left, "not ranked as needed (zazzle get-rules PATH --skill-bodies sends them)"),
    ] {
        if !v.is_empty() {
            out.push_str(&format!("-- {what}: {}\n", names(v)));
        }
    }
    out.trim_end().to_string()
}

// The parsed command line: intent words, paths, purposes and budget.
struct Args {
    intent: String,
    paths: Vec<String>,
    purposes: Vec<String>,
    budget: Option<usize>,
}

// Parses the intent words and the --path, --purpose and --budget options.
fn parse(rest: &[String]) -> Result<Args, String> {
    let mut a = Args { intent: String::new(), paths: Vec::new(), purposes: Vec::new(), budget: None };
    let (mut words, mut it) = (Vec::new(), rest.iter());
    while let Some(x) = it.next() {
        let (flag, inline) = match x.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (x.as_str(), None),
        };
        let mut value =
            || inline.clone().or_else(|| it.next().cloned()).ok_or(format!("{flag} needs a value"));
        match flag {
            "--path" => a.paths.push(value()?),
            "--purpose" => {
                let v = value()?;
                if !purpose::valid(&v) {
                    return Err(format!("not a purpose name: `{v}`"));
                }
                a.purposes.push(v);
            }
            "--budget" => {
                let n = value()?.parse().ok().filter(|&n: &usize| n > 0);
                a.budget = Some(n.ok_or("--budget needs a number")?);
            }
            _ => words.push(x.clone()),
        }
    }
    a.intent = words.join(" ");
    if a.intent.trim().is_empty() {
        return Err(USAGE.into());
    }
    Ok(a)
}

// Returns the purposes a brief command line names, which become the session's active purposes; None when it names none.
pub fn purposes(rest: &[String]) -> Option<Vec<String>> {
    parse(rest).ok().map(|a| a.purposes).filter(|p| !p.is_empty())
}

// Lists what applies to the paths for the purposes (the explicit ones, else the session's), each file and range once, and builds the brief.
pub fn run(rest: &[String], env: Env) -> Result<String, String> {
    let a = parse(rest)?;
    let env = Env {
        purposes: if a.purposes.is_empty() { env.purposes } else { a.purposes.clone() },
        budget: a.budget.unwrap_or(env.budget),
        ..env
    };
    let targets: Vec<PathBuf> = match a.paths.is_empty() {
        true => vec![env.cwd.to_path_buf()],
        false => a.paths.iter().map(|p| env.cwd.join(p)).collect(),
    };
    let found = crate::rules::union(&targets, &env.purposes, env.home)?;
    let scope = if a.paths.is_empty() { ".".to_string() } else { a.paths.join(", ") };
    Ok(brief(&a.intent, &scope, &found, &env))
}

// Asks the real local model at low effort, held to the grammar.
pub fn ask_local(instruction: &str, input: &str, grammar: &str) -> Result<String, String> {
    let user = format!("{instruction}\n\n---\n{input}");
    let req = serde_json::json!({"user": user, "effort": "low", "temp": 0, "grammar": grammar});
    crate::llm::ask(None, &req).map(|r| r.text)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
