// Command-line parsing for humans: the manifest's cli spec turns argv into the JSON input a script receives.
use crate::json::Json;

// The type of one argument's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Bool,
    Count,
    Str,
    Int,
    Float,
    Enum,
    Path,
}

// Whether the script reads standard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdin {
    None,
    Optional,
    Required,
}

// One flag or positional argument.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: String,
    pub short: Option<char>,
    pub long: Option<String>,
    pub ty: Type,
    pub values: Vec<String>,
    pub default: Option<Json>,
    pub required: bool,
    pub multiple: bool,
    pub positional: bool,
    pub help: String,
}

// A whole command-line spec.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub prog: String,
    pub usage: String,
    pub description: String,
    pub stdin: Stdin,
    pub args: Vec<Arg>,
}

// What parsing produced: the input for the script, or help text to print.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Input(Json),
    Help(String),
}

// Reads the type name of an argument.
fn parse_type(s: &str) -> Result<Type, String> {
    Ok(match s {
        "bool" => Type::Bool,
        "count" => Type::Count,
        "string" => Type::Str,
        "int" => Type::Int,
        "float" => Type::Float,
        "enum" => Type::Enum,
        "path" => Type::Path,
        other => return Err(format!("unknown type '{other}' (bool, count, string, int, float, enum, path)")),
    })
}

// Reads one argument spec.
fn parse_arg(v: &Json) -> Result<Arg, String> {
    let name = v["name"].str().ok_or("every arg needs a \"name\"")?.to_string();
    let ok_name = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok_name {
        return Err(format!("arg name '{name}' must be lowercase letters, digits and '_'"));
    }
    let at = |e: String| format!("arg '{name}': {e}");
    let short = match v["short"].str() {
        None => None,
        Some(s) if s.chars().count() == 1 && s != "-" => s.chars().next(),
        Some(s) => return Err(at(format!("short '{s}' must be one character"))),
    };
    let ty = parse_type(v.str_or("type", "string")).map_err(at)?;
    let positional = v["positional"].bool().unwrap_or(false);
    let values: Vec<String> =
        v["values"].arr().unwrap_or(&[]).iter().filter_map(|x| x.str().map(str::to_string)).collect();
    if ty == Type::Enum && values.is_empty() {
        return Err(at("an enum needs \"values\"".into()));
    }
    if positional && matches!(ty, Type::Bool | Type::Count) {
        return Err(at("a positional argument cannot be bool or count".into()));
    }
    let long =
        Some(v.str_or("long", &name.replace('_', "-")).to_string()).filter(|l| !positional && !l.is_empty());
    Ok(Arg {
        short,
        long,
        ty,
        values,
        default: v.get("default").cloned(),
        required: v["required"].bool().unwrap_or(false),
        multiple: v["multiple"].bool().unwrap_or(false),
        positional,
        help: v.str_or("help", "").to_string(),
        name,
    })
}

impl Spec {
    // Reads a spec from the manifest's "cli" object; the program name is the script's name.
    pub fn from_json(prog: &str, v: &Json) -> Result<Spec, String> {
        let stdin = match v.str_or("stdin", "optional") {
            "none" => Stdin::None,
            "optional" => Stdin::Optional,
            "required" => Stdin::Required,
            other => return Err(format!("cli.stdin: '{other}' must be none, optional or required")),
        };
        let args = v["args"].arr().unwrap_or(&[]).iter().map(parse_arg).collect::<Result<Vec<_>, _>>();
        let args = args.map_err(|e| format!("cli: {e}"))?;
        let positionals: Vec<&Arg> = args.iter().filter(|a| a.positional).collect();
        if positionals.iter().rev().skip(1).any(|a| a.multiple) {
            return Err("cli: only the last positional argument can be multiple".into());
        }
        for (i, a) in args.iter().enumerate() {
            let clash = args[..i].iter().any(|b| {
                b.name == a.name
                    || (a.short.is_some() && b.short == a.short)
                    || (a.long.is_some() && b.long == a.long)
            });
            if clash {
                return Err(format!("cli: arg '{}' repeats a name or flag", a.name));
            }
        }
        let usage = v.str_or("usage", "").to_string();
        Ok(Spec {
            prog: prog.to_string(),
            usage,
            description: v.str_or("description", "").to_string(),
            stdin,
            args,
        })
    }

    // True when the spec defines this long flag itself.
    pub fn has_long(&self, long: &str) -> bool {
        self.args.iter().any(|a| a.long.as_deref() == Some(long))
    }

    // The usage line, generated when the spec has none.
    pub fn usage_line(&self) -> String {
        if !self.usage.is_empty() {
            return self.usage.clone();
        }
        let mut parts = vec![self.prog.clone()];
        if self.args.iter().any(|a| !a.positional) {
            parts.push("[OPTIONS]".into());
        }
        for a in self.args.iter().filter(|a| a.positional) {
            let mut p = a.name.to_uppercase();
            if a.multiple {
                p.push_str("...");
            }
            parts.push(if a.required { p } else { format!("[{p}]") });
        }
        parts.join(" ")
    }

    // The full --help text.
    pub fn help(&self) -> String {
        let mut out = format!("usage: {}\n", self.usage_line());
        if !self.description.is_empty() {
            out.push_str(&format!("\n{}\n", self.description));
        }
        let rows: Vec<(String, String)> = self.args.iter().map(|a| (flag_label(a), arg_help(a))).collect();
        let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max(10);
        out.push_str("\narguments:\n");
        for (label, help) in
            rows.iter().chain(std::iter::once(&("-h, --help".to_string(), "show this help".to_string())))
        {
            out.push_str(format!("  {label:width$}  {help}").trim_end());
            out.push('\n');
        }
        out.trim_end().to_string()
    }

    // Finds a flag by its long name.
    fn by_long(&self, long: &str) -> Option<&Arg> {
        self.args.iter().find(|a| a.long.as_deref() == Some(long))
    }

    // Finds a flag by its short letter.
    fn by_short(&self, c: char) -> Option<&Arg> {
        self.args.iter().find(|a| a.short == Some(c))
    }

    // Formats a parse error with the usage line.
    fn fail(&self, msg: &str) -> String {
        format!("{}: {msg}\nusage: {}\ntry: {} --help", self.prog, self.usage_line(), self.prog)
    }

    // Parses argv (without the program name) into the script's input, resolving paths against cwd.
    pub fn parse(&self, argv: &[String], cwd: &str) -> Result<Parsed, String> {
        let mut st = State { out: Json::object(), positional: Vec::new() };
        let mut it = argv.iter();
        while let Some(a) = it.next() {
            if a == "--" {
                st.positional.extend(it.by_ref().cloned());
            } else if (a == "--help" && !self.has_long("help")) || (a == "-h" && self.by_short('h').is_none())
            {
                return Ok(Parsed::Help(self.help()));
            } else if let Some(body) = a.strip_prefix("--") {
                self.long_flag(body, &mut it, cwd, &mut st)?;
            } else if a.len() > 1 && a.starts_with('-') && !is_number(a) {
                self.short_flags(&a[1..], &mut it, cwd, &mut st)?;
            } else {
                st.positional.push(a.clone());
            }
        }
        self.assign_positionals(&mut st, cwd)?;
        self.finish(st.out).map(Parsed::Input)
    }

    // Handles "--name", "--name=value" and "--name value".
    fn long_flag(
        &self,
        body: &str,
        it: &mut std::slice::Iter<String>,
        cwd: &str,
        st: &mut State,
    ) -> Result<(), String> {
        let (name, inline) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (body, None),
        };
        let arg = self.by_long(name).ok_or_else(|| self.fail(&format!("unknown option --{name}")))?;
        if matches!(arg.ty, Type::Bool | Type::Count) {
            if inline.is_some() {
                return Err(self.fail(&format!("--{name} takes no value")));
            }
            return self.flag(arg, st);
        }
        let value = inline
            .or_else(|| it.next().cloned())
            .ok_or_else(|| self.fail(&format!("--{name} needs a value")))?;
        self.store(arg, &value, cwd, st)
    }

    // Handles "-v", "-vvv", "-abc" and "-dVALUE" / "-d VALUE".
    fn short_flags(
        &self,
        cluster: &str,
        it: &mut std::slice::Iter<String>,
        cwd: &str,
        st: &mut State,
    ) -> Result<(), String> {
        for (i, c) in cluster.char_indices() {
            let arg = self.by_short(c).ok_or_else(|| self.fail(&format!("unknown option -{c}")))?;
            if matches!(arg.ty, Type::Bool | Type::Count) {
                self.flag(arg, st)?;
                continue;
            }
            let rest = &cluster[i + c.len_utf8()..];
            let value = if rest.is_empty() { it.next().cloned() } else { Some(rest.to_string()) };
            let value = value.ok_or_else(|| self.fail(&format!("-{c} needs a value")))?;
            return self.store(arg, &value, cwd, st);
        }
        Ok(())
    }

    // Records a bool or count flag.
    fn flag(&self, arg: &Arg, st: &mut State) -> Result<(), String> {
        let v = match arg.ty {
            Type::Count => Json::Int(st.out[arg.name.as_str()].i64().unwrap_or(0) + 1),
            _ => Json::Bool(true),
        };
        st.out.set(&arg.name, v);
        Ok(())
    }

    // Converts and records one value, appending for multiple args.
    fn store(&self, arg: &Arg, raw: &str, cwd: &str, st: &mut State) -> Result<(), String> {
        let v = self.convert(arg, raw, cwd)?;
        if !arg.multiple {
            st.out.set(&arg.name, v);
            return Ok(());
        }
        let mut list = st.out[arg.name.as_str()].arr().map(<[Json]>::to_vec).unwrap_or_default();
        list.push(v);
        st.out.set(&arg.name, Json::Arr(list));
        Ok(())
    }

    // Converts a raw string to the argument's type.
    fn convert(&self, arg: &Arg, raw: &str, cwd: &str) -> Result<Json, String> {
        let bad = |what: &str| self.fail(&format!("{} '{raw}' is not {what}", display_name(arg)));
        Ok(match arg.ty {
            Type::Int => Json::Int(raw.parse().map_err(|_| bad("an integer"))?),
            Type::Float => Json::from(raw.parse::<f64>().map_err(|_| bad("a number"))?),
            Type::Enum if !arg.values.iter().any(|v| v == raw) => {
                return Err(bad(&format!("one of {}", arg.values.join(", "))))
            }
            Type::Path => Json::Str(absolute(raw, cwd)),
            _ => Json::Str(raw.to_string()),
        })
    }

    // Hands collected positional values to the positional args in order.
    fn assign_positionals(&self, st: &mut State, cwd: &str) -> Result<(), String> {
        let mut values = std::mem::take(&mut st.positional).into_iter();
        for arg in self.args.iter().filter(|a| a.positional) {
            if arg.multiple {
                for v in values.by_ref() {
                    self.store(arg, &v, cwd, st)?;
                }
            } else if let Some(v) = values.next() {
                self.store(arg, &v, cwd, st)?;
            }
        }
        match values.next() {
            Some(extra) => Err(self.fail(&format!("unexpected argument '{extra}'"))),
            None => Ok(()),
        }
    }

    // Applies defaults and checks required arguments.
    fn finish(&self, mut out: Json) -> Result<Json, String> {
        for a in &self.args {
            if out.get(&a.name).is_some() {
                continue;
            }
            if a.required {
                return Err(self.fail(&format!("{} is required", display_name(a))));
            }
            let v = match (&a.default, a.ty) {
                (Some(d), _) => d.clone(),
                (None, Type::Bool) => Json::Bool(false),
                (None, Type::Count) => Json::Int(0),
                (None, _) if a.multiple => Json::Arr(Vec::new()),
                (None, _) => Json::Null,
            };
            out.set(&a.name, v);
        }
        Ok(out)
    }
}

// Collected values while parsing.
struct State {
    out: Json,
    positional: Vec<String>,
}

// True for negative numbers, which are values rather than flags.
fn is_number(s: &str) -> bool {
    s.parse::<f64>().is_ok()
}

// Makes a path absolute against the caller's directory; "-" (stdin) stays as it is.
fn absolute(raw: &str, cwd: &str) -> String {
    if raw == "-" || raw.starts_with('/') {
        raw.to_string()
    } else {
        format!("{}/{raw}", cwd.trim_end_matches('/'))
    }
}

// How an argument is named in messages.
fn display_name(a: &Arg) -> String {
    match (&a.long, a.short) {
        (Some(l), _) => format!("--{l}"),
        (None, Some(c)) => format!("-{c}"),
        (None, None) => a.name.to_uppercase(),
    }
}

// The left column of the help table.
fn flag_label(a: &Arg) -> String {
    if a.positional {
        return a.name.to_uppercase();
    }
    let value = if matches!(a.ty, Type::Bool | Type::Count) {
        String::new()
    } else {
        format!(" {}", a.name.to_uppercase())
    };
    match (a.short, &a.long) {
        (Some(c), Some(l)) => format!("-{c}, --{l}{value}"),
        (Some(c), None) => format!("-{c}{value}"),
        (None, Some(l)) => format!("    --{l}{value}"),
        (None, None) => a.name.clone(),
    }
}

// The right column of the help table.
fn arg_help(a: &Arg) -> String {
    let mut h = a.help.clone();
    if !a.values.is_empty() {
        h.push_str(&format!(" [{}]", a.values.join("|")));
    }
    if let Some(d) = &a.default {
        h.push_str(&format!(" [default: {}]", d.str().map_or_else(|| d.to_string(), str::to_string)));
    }
    if a.required {
        h.push_str(" [required]");
    }
    h.trim().to_string()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // A spec with one of each kind of argument.
    fn spec() -> Spec {
        let v = crate::json!({
            "usage": "csv-sum [-d DELIM] [--header] FILE...",
            "description": "Sum columns.",
            "args": [
                {"name": "delim", "short": "d", "long": "delimiter", "default": ",", "help": "field separator"},
                {"name": "header", "long": "header", "type": "bool"},
                {"name": "verbose", "short": "v", "type": "count"},
                {"name": "limit", "short": "n", "type": "int"},
                {"name": "scale", "type": "float"},
                {"name": "format", "type": "enum", "values": ["text", "json"], "default": "text"},
                {"name": "tag", "type": "string", "multiple": true},
                {"name": "out", "type": "path"},
                {"name": "files", "positional": true, "type": "path", "multiple": true, "required": true}
            ]
        });
        Spec::from_json("csv-sum", &v).expect("spec")
    }

    // Parses argv given as one string.
    fn parse(s: &Spec, argv: &str) -> Result<Parsed, String> {
        let v: Vec<String> = argv.split_whitespace().map(str::to_string).collect();
        s.parse(&v, "/work")
    }

    // Parses and unwraps the input.
    fn input(s: &Spec, argv: &str) -> Json {
        match parse(s, argv).expect("parse") {
            Parsed::Input(j) => j,
            Parsed::Help(h) => panic!("unexpected help {h}"),
        }
    }

    // Checks flags, values, counts, multiples, paths and defaults all land in the JSON input.
    #[test]
    fn test_parse_full() {
        let s = spec();
        let j = input(
            &s,
            "-vv --header -d ; -n5 --scale=1.5 --format json --tag a --tag b --out o.txt a.csv /abs.csv -",
        );
        assert_eq!(j["delim"].str(), Some(";"));
        assert_eq!(
            (j["header"].bool(), j["verbose"].i64(), j["limit"].i64()),
            (Some(true), Some(2), Some(5))
        );
        assert_eq!((j["scale"].f64(), j["format"].str()), (Some(1.5), Some("json")));
        assert_eq!(j["tag"], crate::json!(["a", "b"]));
        assert_eq!(j["out"].str(), Some("/work/o.txt"));
        assert_eq!(j["files"], crate::json!(["/work/a.csv", "/abs.csv", "-"]));
        let d = input(&s, "x -- -notaflag");
        assert_eq!(
            (d["delim"].str(), d["header"].bool(), d["verbose"].i64()),
            (Some(","), Some(false), Some(0))
        );
        assert_eq!(
            (d["limit"].clone(), d["tag"].clone(), d["format"].str()),
            (Json::Null, crate::json!([]), Some("text"))
        );
        assert_eq!(d["files"], crate::json!(["/work/x", "/work/-notaflag"]));
        assert_eq!(input(&s, "-n -3 f")["limit"].i64(), Some(-3));
        assert_eq!(input(&s, "-vd: f")["delim"].str(), Some(":"));
    }

    // Checks every parse error names the problem and shows usage.
    #[test]
    fn test_parse_errors() {
        let s = spec();
        for (argv, want) in [
            ("", "FILES is required"),
            ("--nope f", "unknown option --nope"),
            ("-x f", "unknown option -x"),
            ("--header=1 f", "--header takes no value"),
            ("f --delimiter", "--delimiter needs a value"),
            ("f -d", "-d needs a value"),
            ("-n x f", "'x' is not an integer"),
            ("--scale x f", "'x' is not a number"),
            ("--format xml f", "one of text, json"),
        ] {
            let e = parse(&s, argv).unwrap_err();
            assert!(e.contains(want) && e.contains("usage: csv-sum"), "{argv}: {e}");
        }
        let one =
            Spec::from_json("p", &crate::json!({"args": [{"name": "a", "positional": true}]})).expect("one");
        assert!(parse(&one, "x y").unwrap_err().contains("unexpected argument 'y'"));
        assert_eq!(input(&one, "x")["a"].str(), Some("x"));
        assert_eq!(input(&one, "")["a"], Json::Null);
    }

    // Checks help is shown for -h/--help and lists every argument.
    #[test]
    fn test_help() {
        let s = spec();
        let Parsed::Help(h) = parse(&s, "--help").expect("help") else { panic!("not help") };
        assert!(h.starts_with("usage: csv-sum [-d DELIM]"));
        assert!(h.contains("-d, --delimiter DELIM  field separator [default: ,]"));
        assert!(h.contains("--format FORMAT") && h.contains("[text|json] [default: text]"));
        assert!(h.contains("FILES") && h.contains("[required]") && h.contains("Sum columns."));
        assert_eq!(parse(&s, "-h").expect("h"), Parsed::Help(h));
        let own = Spec::from_json(
            "p",
            &crate::json!({"args": [{"name": "host", "short": "h"}, {"name": "help", "type": "bool"}]}),
        )
        .expect("own");
        assert_eq!(input(&own, "-h x --help")["help"].bool(), Some(true));
        assert!(own.has_long("help") && !own.has_long("json"));
    }

    // Checks the generated usage line.
    #[test]
    fn test_usage_line_generated() {
        let s = Spec::from_json(
            "p",
            &crate::json!({"args": [{"name": "q", "type": "bool"}, {"name": "a", "positional": true, "required": true}, {"name": "rest", "positional": true, "multiple": true}]}),
        )
        .expect("spec");
        assert_eq!(s.usage_line(), "p [OPTIONS] A [REST...]");
        assert!(s.help().contains("    --q"));
        let bare = Spec::from_json("p", &crate::json!({})).expect("bare");
        assert_eq!((bare.usage_line(), bare.stdin), ("p".to_string(), Stdin::Optional));
        let short_only =
            Spec::from_json("p", &crate::json!({"args": [{"name": "x", "short": "x", "long": ""}]}))
                .expect("s");
        assert!(short_only.help().contains("  -x X\n"));
    }

    // Checks invalid specs are refused with the field named.
    #[test]
    fn test_spec_errors() {
        for (v, want) in [
            (crate::json!({"stdin": "maybe"}), "cli.stdin"),
            (crate::json!({"args": [{}]}), "needs a \"name\""),
            (crate::json!({"args": [{"name": "Bad"}]}), "lowercase"),
            (crate::json!({"args": [{"name": "a", "short": "ab"}]}), "one character"),
            (crate::json!({"args": [{"name": "a", "short": "-"}]}), "one character"),
            (crate::json!({"args": [{"name": "a", "type": "date"}]}), "unknown type"),
            (crate::json!({"args": [{"name": "a", "type": "enum"}]}), "needs \"values\""),
            (crate::json!({"args": [{"name": "a", "type": "bool", "positional": true}]}), "cannot be bool"),
            (
                crate::json!({"args": [{"name": "a", "positional": true, "multiple": true}, {"name": "b", "positional": true}]}),
                "only the last",
            ),
            (crate::json!({"args": [{"name": "a"}, {"name": "a"}]}), "repeats"),
            (crate::json!({"args": [{"name": "a", "short": "x"}, {"name": "b", "short": "x"}]}), "repeats"),
        ] {
            let e = Spec::from_json("p", &v).unwrap_err();
            assert!(e.contains(want), "{v}: {e}");
        }
        assert!(Spec::from_json("p", &crate::json!({"args": [{"name": "a_1"}]})).is_ok());
        let s = Spec::from_json("p", &crate::json!({"stdin": "none"})).expect("none");
        assert_eq!(s.stdin, Stdin::None);
        let r = Spec::from_json("p", &crate::json!({"stdin": "required"})).expect("req");
        assert_eq!(r.stdin, Stdin::Required);
    }

    // Checks the small helpers.
    #[test]
    fn test_helpers() {
        assert_eq!(absolute("a", "/w/"), "/w/a");
        assert_eq!(absolute("-", "/w"), "-");
        let a = Arg {
            name: "x".into(),
            short: None,
            long: None,
            ty: Type::Str,
            values: vec![],
            default: Some(Json::Int(3)),
            required: false,
            multiple: false,
            positional: false,
            help: String::new(),
        };
        assert_eq!(
            (display_name(&a), flag_label(&a), arg_help(&a)),
            ("X".to_string(), "x".to_string(), "[default: 3]".to_string())
        );
        let c = Arg { short: Some('c'), ..a.clone() };
        assert_eq!((display_name(&c), flag_label(&c)), ("-c".to_string(), "-c X".to_string()));
    }
}
