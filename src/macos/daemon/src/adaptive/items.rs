// The items a task asks for, each with a stable id (I1, I2) and its own words, and the constraints on the whole answer (its format and length), so a planner assigns items instead of rewording them; and the check of a plan against them before anything is dispatched.
use regex::Regex;
use std::sync::LazyLock;

// Words that make a sentence a constraint on the answer rather than something to find out.
const FORMATS: &[&str] = &[
    "as a table",
    "in a table",
    "a table of",
    "call chain",
    "test matrix",
    "as a list",
    "ordered list",
    "bullet",
    "at most",
    "no more than",
    "word limit",
    " words",
    "as json",
    "in json",
];

// A list marker at the start of a line: `1.`, `2)`, `-`, `*`, `•` or `a)`.
static MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:\d+[.)]|[-*•]|[a-z][.)])\s+").expect("valid regex"));
// A plan line's starting symbol: a name, a qualified name, or a handle.
static SEED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_.:]*$").expect("valid regex"));

// One requested item: its id and its words, as the task gave them.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub text: String,
}

// What a task asks for: its items and the constraints on the whole answer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Task {
    pub items: Vec<Item>,
    pub constraints: Vec<String>,
}

// One validated plan line: the rung (0-based), the items it answers and its starting symbols.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub rung: usize,
    pub items: Vec<String>,
    pub seeds: Vec<String>,
}

// Whether a sentence constrains the answer's format or length.
fn constraint(s: &str) -> bool {
    let lower = s.to_lowercase();
    FORMATS.iter().any(|f| lower.contains(f))
}

// Splits text into sentences, each keeping its end mark.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        cur.push(*c);
        let end = matches!(c, '?' | '.' | ';' | '!') && chars.get(i + 1).is_none_or(|n| n.is_whitespace());
        if end && !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

// Reads a task: marked list lines are its items when there are two or more (other lines are items too when they ask a question, else context or constraints); else each question sentence is an item, and other sentences are constraints when they set the format or length, else items too; with nothing found, the whole text is the one item.
pub fn task(question: &str) -> Task {
    let lines: Vec<&str> = question.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let marked: Vec<&str> = lines.iter().copied().filter(|l| MARKER.is_match(l)).collect();
    let (mut items, mut constraints) = (Vec::new(), Vec::new());
    if marked.len() >= 2 {
        for l in &lines {
            match MARKER.is_match(l) {
                true => items.push(MARKER.replace(l, "").trim().to_string()),
                false if l.ends_with('?') => items.push(l.to_string()),
                false => constraints.push(l.to_string()),
            }
        }
    } else {
        for s in sentences(&lines.join(" ")) {
            match (s.ends_with('?'), constraint(&s)) {
                (false, true) => constraints.push(s),
                _ => items.push(s),
            }
        }
    }
    if items.is_empty() {
        items.push(question.trim().to_string());
    }
    Task {
        items: items
            .into_iter()
            .enumerate()
            .map(|(i, text)| Item { id: format!("I{}", i + 1), text })
            .collect(),
        constraints,
    }
}

impl Task {
    // The items as the planner sees them, one `I1: text` line each, then the constraints.
    pub fn listing(&self) -> String {
        let mut out: String = self.items.iter().map(|i| format!("{}: {}\n", i.id, i.text)).collect();
        if !self.constraints.is_empty() {
            out.push_str(&format!("Constraints on the whole answer: {}\n", self.constraints.join(" ")));
        }
        out
    }

    // What an executor is asked for some items: their own words, then the constraints on the answer.
    pub fn text_of(&self, ids: &[String]) -> String {
        let mut out: Vec<String> = self
            .items
            .iter()
            .filter(|i| ids.contains(&i.id))
            .map(|i| format!("{}: {}", i.id, i.text))
            .collect();
        if !self.constraints.is_empty() {
            out.push(format!("Constraints on the answer: {}", self.constraints.join(" ")));
        }
        out.join("\n")
    }
}

// The word limit the constraints set, if any: "at most N words", "no more than N words", "under N words", "in N words".
pub fn word_limit(constraints: &[String]) -> Option<usize> {
    static LIMIT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:at most|no more than|under|within|in)\s+(\d+)\s+words").expect("valid regex")
    });
    constraints.iter().find_map(|c| LIMIT.captures(c).and_then(|m| m[1].parse().ok()))
}

// The grammar a local planner is held to: PART lines naming an agent number, item ids and starting symbols, then the CONFIDENCE line.
pub const GRAMMAR: &str = r#"root ::= part+ conf
part ::= "PART " [1-9] " | " item ("," " "? item)* " | " seeds "\n"
item ::= "I" [0-9]+
seeds ::= [^\n|]*
conf ::= "CONFIDENCE understanding=" lvl " result=" lvl "\n"?
lvl ::= "high" | "medium" | "low"
"#;

// Checks a planner's reply against the task and the ladder before anything is dispatched: each PART line must name an agent on the ladder, only known items and starting symbols that are names, there are at most `max_parts` parts, and every item must be in some part; a reply cut short is refused. Returns the assignments, or every problem found.
pub fn check(
    reply: &str,
    task: &Task,
    rungs: usize,
    max_parts: usize,
    truncated: bool,
) -> Result<Vec<Assignment>, Vec<String>> {
    let mut problems = Vec::new();
    if truncated {
        problems.push("the plan was cut short".to_string());
    }
    let mut out = Vec::new();
    for l in reply.lines().map(str::trim) {
        let Some(rest) = crate::investigate::cite::strip_word(l, "PART") else { continue };
        let fields: Vec<&str> = rest.split('|').map(str::trim).collect();
        let rung = match fields.first().and_then(|f| f.trim_end_matches(['.', ':']).parse::<usize>().ok()) {
            Some(n) if (1..=rungs).contains(&n) => n - 1,
            Some(n) => {
                problems.push(format!("`{l}`: agent {n} is not on the ladder (1 to {rungs})"));
                continue;
            }
            None => {
                problems.push(format!("`{l}`: the first field must be an agent number"));
                continue;
            }
        };
        let ids: Vec<String> = fields
            .get(1)
            .copied()
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if ids.is_empty() {
            problems.push(format!("`{l}`: the second field must name item ids such as I1"));
            continue;
        }
        if let Some(bad) = ids.iter().find(|i| !task.items.iter().any(|t| t.id == **i)) {
            problems.push(format!("`{l}`: `{bad}` is not an item of the task"));
            continue;
        }
        let seeds: Vec<String> = fields
            .get(2)
            .copied()
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if let Some(bad) = seeds.iter().find(|s| !SEED.is_match(s)) {
            problems.push(format!("`{l}`: starting symbol `{bad}` is not a name"));
            continue;
        }
        out.push(Assignment { rung, items: ids, seeds });
    }
    if out.len() > max_parts {
        problems.push(format!("the plan has {} parts; at most {max_parts} are allowed", out.len()));
    }
    if out.is_empty() && problems.is_empty() {
        problems.push("the reply had no PART lines".into());
    }
    for item in &task.items {
        if !out.iter().any(|a| a.items.contains(&item.id)) && !problems.iter().any(|p| p.contains("no PART"))
        {
            problems.push(format!("{} is in no part", item.id));
        }
    }
    match problems.is_empty() {
        true => Ok(out),
        false => Err(problems),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks items come from list lines, else question sentences, with format and length sentences kept as constraints, and the whole text as one item when nothing else is found.
    #[test]
    fn test_task_items() {
        let t = task("For the station cache:\n1. What is the dictionary key?\n2. When is the stored question compared?\n- When does the memory metric go up?\nAnswer as a table.");
        assert_eq!(
            t.items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            [
                "What is the dictionary key?",
                "When is the stored question compared?",
                "When does the memory metric go up?"
            ]
        );
        assert_eq!(t.constraints, ["For the station cache:", "Answer as a table."]);
        let t =
            task("1. What is the key?\n2. When is it compared?\nAlso, why is the cache cleared on restart?");
        assert_eq!(
            (t.items.len(), t.items[2].text.as_str()),
            (3, "Also, why is the cache cleared on restart?"),
            "an unmarked question is still an item"
        );
        let t = task("Where is the key checked? Should a blank key be rejected? Give a test matrix in at most 200 words.");
        assert_eq!(t.items.len(), 2);
        assert_eq!(t.constraints, ["Give a test matrix in at most 200 words."]);
        assert_eq!(t.items[1].id, "I2");
        let t = task("Trace a wash request from its endpoint through eligibility and persistence");
        assert_eq!(
            (t.items.len(), t.items[0].text.as_str()),
            (1, "Trace a wash request from its endpoint through eligibility and persistence")
        );
        assert_eq!(
            task("Answer as a table.").items[0].text,
            "Answer as a table.",
            "nothing to find out: the whole text is the item"
        );
        let t = task("What is the key? Answer in a table.");
        assert!(t
            .listing()
            .contains("I1: What is the key?\nConstraints on the whole answer: Answer in a table.\n"));
        assert_eq!(
            t.text_of(&["I1".into()]),
            "I1: What is the key?\nConstraints on the answer: Answer in a table."
        );
    }

    // Checks a plan is refused for numeric or model-label questions, agents off the ladder, unknown items, symbols that are not names, an uncovered item and a cut-short reply, and accepted when it assigns every item.
    #[test]
    fn test_plan_check() {
        let t = task("What is the key? When is the question compared? What does a miss do?");
        let ok = check(
            "PART 1 | I1, I2 | StationCache\nPART 2 | I3 |\nCONFIDENCE understanding=high result=high",
            &t,
            2,
            4,
            false,
        )
        .expect("valid");
        assert_eq!(
            ok[0],
            Assignment { rung: 0, items: vec!["I1".into(), "I2".into()], seeds: vec!["StationCache".into()] }
        );
        assert_eq!((ok[1].rung, ok[1].seeds.len()), (1, 0));
        let bad = check("PART 1 | 2 | x\nPART 1 | qwen3.5-4b low | Where?\nPART 7 | I1 |\nPART x | I1 |\nPART 1 | I9 |\nPART 1 | |\nPART 1 | I2 | the cache key", &t, 2, 4, true).unwrap_err();
        for want in [
            "cut short",
            "`2` is not an item",
            "`qwen3.5-4b low` is not an item",
            "agent 7 is not on the ladder (1 to 2)",
            "must be an agent number",
            "`I9` is not an item",
            "must name item ids",
            "`the cache key` is not a name",
            "I1 is in no part",
            "I3 is in no part",
        ] {
            assert!(bad.iter().any(|p| p.contains(want)), "{want}: {bad:?}");
        }
        assert_eq!(check("no plan here", &t, 2, 4, false).unwrap_err(), ["the reply had no PART lines"]);
        assert_eq!(
            check("PART 1 | I1 |\nPART 1 | I2 |\nPART 2 | I3 |", &t, 2, 2, false).unwrap_err(),
            ["the plan has 3 parts; at most 2 are allowed"],
            "a part past the limit would never run, so its items would go unanswered"
        );
    }
}
