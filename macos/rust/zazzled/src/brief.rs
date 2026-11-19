// `brief INTENT`: the rule sections and skills an agent needs for what it is about to do, chosen by the local model from everything get-rules would send and printed verbatim, so the agent reads only those.
use regex::Regex;
use std::path::{Path, PathBuf};
use zazzle_agent_rules_list::{body, list_rules, Found, Kind};

const MENU_CHARS: usize = 2500;

// The MCP tool for brief.
pub(crate) const TOOLS: &[crate::mcp::Tool] = &[crate::mcp::Tool {
    name: "zazzle_brief",
    description: "Before a task: the rule sections and skills that apply to what you intend to do, picked by the local model from all that applies to the paths and printed verbatim; what was left out is named in one line.",
    schema,
    map: args,
}];

pub const USAGE: &str = "usage: brief INTENT.. [--path PATH]..";

// Schema for zazzle_brief.
fn schema() -> serde_json::Value {
    crate::mcp::object(
        serde_json::json!({
            "intent": {"type": "string", "description": "What you are about to do, in a sentence"},
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Files or folders the work touches (default: the working directory)"}
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
    Ok(a)
}

// One piece an agent may need: a rule file's lines under one heading, or a whole skill.
#[derive(Debug, PartialEq)]
struct Section {
    kind: Kind,
    path: PathBuf,
    lines: (usize, usize),
    heading: String,
    text: String,
    menu: String,
}

// Splits a rule or import file at its Markdown headings (outside code fences) into sections of verbatim lines.
fn split(f: &Found, text: &str) -> Vec<Section> {
    let lines: Vec<&str> = text.lines().collect();
    let (mut starts, mut fence) = (vec![0], false);
    for (i, l) in lines.iter().enumerate() {
        fence ^= l.trim_start().starts_with("```");
        if !fence && i > 0 && l.starts_with('#') {
            starts.push(i);
        }
    }
    starts.push(lines.len());
    starts
        .windows(2)
        .filter_map(|w| {
            let part = lines[w[0]..w[1]].join("\n");
            let heading = lines[w[0]].trim_start_matches('#').trim().to_string();
            (!part.trim().is_empty()).then(|| Section {
                kind: f.kind,
                path: f.path.clone(),
                lines: (w[0] + 1, w[1]),
                heading,
                menu: part.chars().take(MENU_CHARS).collect(),
                text: part,
            })
        })
        .collect()
}

// Turns the found files into sections: rules and imports by heading first, then each skill whole, offered by its description (rules first, since small models favour the first items).
fn sections(found: &[Found]) -> Vec<Section> {
    let mut out = Vec::new();
    for f in found.iter().filter(|f| f.lines.is_empty()) {
        let Some(text) = f.text.as_deref() else { continue };
        if f.kind != Kind::Skill {
            out.extend(split(f, text));
            continue;
        }
        let desc = body(f, false).lines().skip(1).collect::<Vec<_>>().join(" ");
        let n = text.lines().count();
        out.push(Section {
            kind: f.kind,
            path: f.path.clone(),
            lines: (1, n),
            heading: String::new(),
            text: text.to_string(),
            menu: desc,
        });
    }
    out.sort_by_key(|x| x.kind == Kind::Skill);
    out
}

// Shows a path relative to the working directory when it is under it.
fn shown(p: &Path, cwd: &Path) -> String {
    p.strip_prefix(cwd).unwrap_or(p).display().to_string()
}

// Names a section in one line: kind, path and heading.
fn title(s: &Section, cwd: &Path) -> String {
    let h = if s.heading.is_empty() { String::new() } else { format!(" § {}", s.heading) };
    format!("{} {}{h}", s.kind.name(), shown(&s.path, cwd))
}

// Builds what the local model reads: the numbered sections, each named and shortened.
fn menu(all: &[Section], cwd: &Path) -> String {
    all.iter()
        .enumerate()
        .map(|(i, s)| format!("[{}] {}\n{}\n\n", i + 1, title(s, cwd), s.menu.trim()))
        .collect()
}

// A GBNF grammar that makes the model judge every section in turn: `1:y 2:n ..` up to n, so a small model cannot stop early or ramble.
fn verdicts(n: usize) -> String {
    let items: Vec<String> = (1..=n).map(|k| format!("\"{k}:\" v")).collect();
    format!("root ::= {}\nv ::= \"y\" | \"n\"", items.join(" \" \" "))
}

// Reads the sections the model said yes to, in order, each once and in range.
fn chosen(answer: &str, n: usize) -> Vec<usize> {
    static YES: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = YES.get_or_init(|| Regex::new(r"(\d+):y").expect("valid regex"));
    let mut out: Vec<usize> = Vec::new();
    for c in re.captures_iter(answer) {
        if let Ok(k) = c[1].parse::<usize>() {
            if (1..=n).contains(&k) && !out.contains(&(k - 1)) {
                out.push(k - 1);
            }
        }
    }
    out.sort_unstable();
    out
}

// Prints the chosen sections verbatim, then names the rest in one line.
fn render(intent: &str, all: &[Section], pick: &[usize], cwd: &Path) -> String {
    let mut out = format!(
        "brief for: {intent}\n{} of {} sections, chosen by the local model\n\n",
        pick.len(),
        all.len()
    );
    for &i in pick {
        let s = &all[i];
        let range =
            if s.kind == Kind::Skill { String::new() } else { format!(":{}-{}", s.lines.0, s.lines.1) };
        out.push_str(&format!(
            "==> {} {}{range} <==\n{}\n\n",
            s.kind.name(),
            shown(&s.path, cwd),
            s.text.trim_end()
        ));
    }
    let rest: Vec<String> =
        (0..all.len()).filter(|i| !pick.contains(i)).map(|i| title(&all[i], cwd)).collect();
    if !rest.is_empty() {
        out.push_str(&format!(
            "left out: {} (zazzle get-rules PATH --again sends everything)\n",
            rest.join("; ")
        ));
    }
    out.trim_end().to_string()
}

// The question put to the local model.
fn instruction(intent: &str) -> String {
    format!(
        "An agent is about to do this task: {intent}\n\nBelow are numbered sections of the rule files and skills that apply where it works. For each section write its number, a colon, then y if the agent needs it for this task or n if not. Say y to rules it must follow while doing the task, including general rules for all work in the repository, and to skills that describe how to do the task; n to the rest."
    )
}

// Builds a brief from found files with a way to ask the model; an unusable answer falls back to every section.
fn brief(
    intent: &str,
    found: &[Found],
    cwd: &Path,
    ask: &dyn Fn(&str, &str, &str) -> Result<String, String>,
) -> Result<String, String> {
    let all = sections(found);
    if all.is_empty() {
        return Ok(format!("brief for: {intent}\nno rule or skill files apply here"));
    }
    let input = format!("{}The task again: {intent}\nVerdicts:", menu(&all, cwd));
    let answer = ask(&instruction(intent), &input, &verdicts(all.len()))?;
    Ok(render(intent, &all, &chosen(&answer, all.len()), cwd))
}

// Parses the intent words and --path options.
fn parse(rest: &[String]) -> Result<(String, Vec<String>), String> {
    let (mut words, mut paths) = (Vec::new(), Vec::new());
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if a == "--path" {
            paths.push(it.next().ok_or("--path needs a path")?.clone());
        } else {
            words.push(a.clone());
        }
    }
    let intent = words.join(" ");
    if intent.trim().is_empty() {
        return Err(USAGE.into());
    }
    Ok((intent, paths))
}

// Finds what applies to the paths (each file once) and asks the local model which parts the intent needs.
pub fn run(
    rest: &[String],
    cwd: &Path,
    home: Option<&Path>,
    ask: &dyn Fn(&str, &str, &str) -> Result<String, String>,
) -> Result<String, String> {
    let (intent, paths) = parse(rest)?;
    let targets: Vec<PathBuf> =
        if paths.is_empty() { vec![cwd.to_path_buf()] } else { paths.iter().map(|p| cwd.join(p)).collect() };
    let mut found: Vec<Found> = Vec::new();
    for t in &targets {
        for f in list_rules(t, home, &[], Path::new("/"))? {
            if !found.iter().any(|g| g.path == f.path) {
                found.push(f);
            }
        }
    }
    brief(&intent, &found, cwd, ask)
        .map_err(|e| format!("{e}; zazzle get-rules PATH sends the rules without the local model"))
}

// Asks the real local model at low effort, held to the grammar.
pub fn ask_local(instruction: &str, input: &str, grammar: &str) -> Result<String, String> {
    crate::llm::ask(instruction, input, &["--effort", "low", "--temp", "0", "--grammar", grammar])
        .map(|r| r.text)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // A rule file with a preamble, two headings and a fenced heading-like line, and a skill.
    fn found(t: &TempDir) -> Vec<Found> {
        let rule =
            "# Repo\nIntro line.\n## Build\ncargo build\n```\n# not a heading\n```\n## Release\nTag it.\n";
        let skill =
            "---\nname: deploy\ndescription: Deploy the site to production\n---\n# Deploy\nRun deploy.sh.\n";
        vec![
            Found {
                kind: Kind::Rule,
                path: t.write("AGENTS.md", rule.as_bytes()),
                lines: Vec::new(),
                text: Some(rule.into()),
            },
            Found {
                kind: Kind::Skill,
                path: t.write("skills/deploy/SKILL.md", skill.as_bytes()),
                lines: Vec::new(),
                text: Some(skill.into()),
            },
        ]
    }

    // Checks rules split at headings outside fences, skills stay whole and are offered by their description.
    #[test]
    fn test_brief_sections() {
        let t = TempDir::new();
        let s = sections(&found(&t));
        let heads: Vec<(&str, (usize, usize))> = s.iter().map(|x| (x.heading.as_str(), x.lines)).collect();
        assert_eq!(heads, vec![("Repo", (1, 2)), ("Build", (3, 7)), ("Release", (8, 9)), ("", (1, 6))]);
        assert!(s[1].text.contains("# not a heading") && s[3].menu.contains("Deploy the site"));
        let m = menu(&s, t.path());
        assert!(
            m.starts_with("[1] rule AGENTS.md § Repo\n# Repo\nIntro line.")
                && m.contains("[4] skill skills/deploy/SKILL.md\n")
        );
    }

    // Checks the chosen sections print verbatim with line ranges, skills whole, the rest named, and odd answers handled.
    #[test]
    fn test_brief_render() {
        let t = TempDir::new();
        let f = found(&t);
        let out = brief("ship it", &f, t.path(), &|i, _, g| {
            assert!(i.contains("ship it") && g == verdicts(4));
            Ok("1:n 2:n 3:y 4:y 3:y 99:y".into())
        })
        .expect("brief");
        assert!(out.starts_with("brief for: ship it\n2 of 4 sections"), "{out}");
        assert!(out.contains("==> rule AGENTS.md:8-9 <==\n## Release\nTag it.\n\n==> skill skills/deploy/SKILL.md <==\n---\nname: deploy"));
        assert!(out.ends_with("left out: rule AGENTS.md § Repo; rule AGENTS.md § Build (zazzle get-rules PATH --again sends everything)"));
        let none = brief("x", &f, t.path(), &|_, _, _| Ok("1:n 2:n 3:n 4:n".into())).expect("none");
        assert!(none.contains("0 of 4") && none.contains("left out: rule AGENTS.md § Repo;"));
        assert_eq!(brief("x", &f, t.path(), &|_, _, _| Err("down".into())), Err("down".into()));
        assert!(brief("x", &[], t.path(), &|_, _, _| unreachable!())
            .expect("none")
            .ends_with("no rule or skill files apply here"));
        assert_eq!(chosen("2:y 2:y 1:y 3:n 7:y", 3), vec![0, 1]);
        assert_eq!(verdicts(2), "root ::= \"1:\" v \" \" \"2:\" v\nv ::= \"y\" | \"n\"");
    }

    // Checks the command line: intent words, --path, the usage error, and lookup through the real lister.
    #[test]
    fn test_brief_run() {
        let t = TempDir::new();
        found(&t);
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse(&v(&["fix", "the", "build", "--path", "src"])),
            Ok(("fix the build".into(), v(&["src"])))
        );
        assert_eq!(parse(&v(&["--path"])), Err("--path needs a path".into()));
        assert_eq!(parse(&v(&[])), Err(USAGE.into()));
        let out = run(&v(&["release", "--path", "."]), t.path(), None, &|_, menu, _| {
            Ok(if menu.contains("[3] rule AGENTS.md § Release") {
                "1:n 2:n 3:y".into()
            } else {
                String::new()
            })
        })
        .expect("run");
        assert!(out.contains("==> rule AGENTS.md:8-9 <==\n## Release"), "{out}");
        let e = run(&v(&["x"]), t.path(), None, &|_, _, _| Err("local model: off".into())).expect_err("down");
        assert!(e.ends_with("sends the rules without the local model"));
        assert!(
            args(&serde_json::json!({"intent": "go", "paths": ["a"]})).expect("args")
                == v(&["brief", "go", "--path", "a"])
        );
    }
}
