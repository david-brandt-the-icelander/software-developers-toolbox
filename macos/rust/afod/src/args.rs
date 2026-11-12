// Minimal command-line parsing with terse one-line errors for agents.

// One accepted flag: long name, short letter ('\0' for none) and whether it takes a value.
pub struct Flag {
    pub long: &'static str,
    pub short: char,
    pub value: bool,
}

// Declares a boolean flag.
pub const fn flag(long: &'static str, short: char) -> Flag {
    Flag { long, short, value: false }
}

// Declares a flag that takes a value.
pub const fn opt(long: &'static str, short: char) -> Flag {
    Flag { long, short, value: true }
}

// The result of parsing: positional arguments and the flags seen, in order.
#[derive(Debug, Default)]
pub struct Parsed {
    pub positional: Vec<String>,
    seen: Vec<(&'static str, Option<String>)>,
}

impl Parsed {
    // Returns true if the flag was given.
    pub fn has(&self, long: &str) -> bool {
        self.seen.iter().any(|(l, _)| *l == long)
    }

    // Returns the last value given for the flag.
    pub fn value(&self, long: &str) -> Option<&str> {
        self.seen.iter().rev().find(|(l, _)| *l == long).and_then(|(_, v)| v.as_deref())
    }

    // Returns every value given for the flag, in order.
    pub fn values(&self, long: &str) -> Vec<&str> {
        self.seen.iter().filter(|(l, _)| *l == long).filter_map(|(_, v)| v.as_deref()).collect()
    }

    // Parses the flag's value as a number.
    pub fn number(&self, long: &str) -> Result<Option<usize>, String> {
        let Some(v) = self.value(long) else { return Ok(None) };
        v.parse::<usize>().map(Some).map_err(|_| format!("--{long} needs a number, got '{v}'"))
    }
}

// Lists the known flags for an error message.
fn known(spec: &[Flag]) -> String {
    spec.iter().map(|f| format!("--{}", f.long)).collect::<Vec<_>>().join(" ")
}

// Looks up a flag by its long name.
fn by_long<'a>(spec: &'a [Flag], name: &str) -> Result<&'a Flag, String> {
    spec.iter()
        .find(|f| f.long == name)
        .ok_or_else(|| format!("unknown flag --{name}; known: {}", known(spec)))
}

// Looks up a flag by its short letter.
fn by_short(spec: &[Flag], c: char) -> Result<&Flag, String> {
    spec.iter().find(|f| f.short == c).ok_or_else(|| format!("unknown flag -{c}; known: {}", known(spec)))
}

// Parses arguments against the flag spec.
pub fn parse(spec: &[Flag], args: &[String]) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if a == "--" {
            out.positional.extend(args[i..].iter().cloned());
            break;
        }
        if let Some(rest) = a.strip_prefix("--") {
            let (name, inline) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (rest, None),
            };
            let f = by_long(spec, name)?;
            let value = take_value(f, inline, args, &mut i)?;
            out.seen.push((f.long, value));
        } else if a.len() > 1 && a.starts_with('-') {
            let chars: Vec<char> = a[1..].chars().collect();
            let mut k = 0;
            while k < chars.len() {
                let f = by_short(spec, chars[k])?;
                k += 1;
                if f.value {
                    let tail: String = chars[k..].iter().collect();
                    let inline = if tail.is_empty() { None } else { Some(tail) };
                    let value = take_value(f, inline, args, &mut i)?;
                    out.seen.push((f.long, value));
                    break;
                }
                out.seen.push((f.long, None));
            }
        } else {
            out.positional.push(a.clone());
        }
    }
    Ok(out)
}

// Gets the value for a flag from its inline part or the next argument.
fn take_value(
    f: &Flag,
    inline: Option<String>,
    args: &[String],
    i: &mut usize,
) -> Result<Option<String>, String> {
    if !f.value {
        return match inline {
            Some(_) => Err(format!("--{} takes no value", f.long)),
            None => Ok(None),
        };
    }
    if inline.is_some() {
        return Ok(inline);
    }
    let Some(v) = args.get(*i) else { return Err(format!("--{} needs a value", f.long)) };
    *i += 1;
    Ok(Some(v.clone()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const SPEC: &[Flag] = &[flag("word", 'w'), flag("fixed", 'F'), opt("context", 'C'), opt("glob", 'g')];

    // Converts string slices into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks positional arguments and boolean flags.
    #[test]
    fn test_parse_positional_and_bool_flags() {
        let p = parse(SPEC, &v(&["foo", "-w", "src"])).expect("parse");
        assert_eq!(p.positional, vec!["foo", "src"]);
        assert!(p.has("word") && !p.has("fixed"));
    }

    // Checks combined short flags and attached short values.
    #[test]
    fn test_parse_combined_shorts_and_attached_value() {
        let p = parse(SPEC, &v(&["-wFC3"])).expect("parse");
        assert!(p.has("word") && p.has("fixed"));
        assert_eq!(p.number("context"), Ok(Some(3)));
    }

    // Checks separate and inline long values and repeated flags.
    #[test]
    fn test_parse_long_values_and_repeats() {
        let p = parse(SPEC, &v(&["--glob", "*.rs", "--glob=*.md", "--context", "2"])).expect("parse");
        assert_eq!(p.values("glob"), vec!["*.rs", "*.md"]);
        assert_eq!(p.value("glob"), Some("*.md"));
        assert_eq!(p.number("context"), Ok(Some(2)));
    }

    // Checks that a short value flag takes the next argument.
    #[test]
    fn test_parse_short_value_from_next_arg() {
        let p = parse(SPEC, &v(&["-g", "x", "-C", "4"])).expect("parse");
        assert_eq!(p.value("glob"), Some("x"));
        assert_eq!(p.number("context"), Ok(Some(4)));
    }

    // Checks that -- ends flag parsing and a lone dash is positional.
    #[test]
    fn test_parse_double_dash_and_lone_dash() {
        let p = parse(SPEC, &v(&["-", "--", "-w", "--x"])).expect("parse");
        assert_eq!(p.positional, vec!["-", "-w", "--x"]);
        assert!(!p.has("word"));
    }

    // Checks the error messages for unknown and malformed flags.
    #[test]
    fn test_parse_errors() {
        assert!(parse(SPEC, &v(&["--nope"])).unwrap_err().starts_with("unknown flag --nope; known: --word"));
        assert!(parse(SPEC, &v(&["-z"])).unwrap_err().starts_with("unknown flag -z"));
        assert_eq!(parse(SPEC, &v(&["--word=1"])).unwrap_err(), "--word takes no value");
        assert_eq!(parse(SPEC, &v(&["--glob"])).unwrap_err(), "--glob needs a value");
        assert_eq!(parse(SPEC, &v(&["-g"])).unwrap_err(), "--glob needs a value");
    }

    // Checks number parsing failures and absence.
    #[test]
    fn test_number_errors_and_missing() {
        let p = parse(SPEC, &v(&["-C", "x"])).expect("parse");
        assert_eq!(p.number("context").unwrap_err(), "--context needs a number, got 'x'");
        assert_eq!(p.number("glob"), Ok(None));
        assert_eq!(p.value("fixed"), None);
    }
}
