// One spelling for scope and limits across the search commands, so a flag that works on one works on the others instead of costing a retry: `--path` scopes find, refs, outline and find-symbol; `--max` and `--limit` both cap results.

// Rewrites a search command's arguments into the spelling it parses: for find and refs `--path P` becomes a trailing path and `--limit N` becomes `--max N`; for find-symbol `--max N` becomes `--limit N` and `-g`/`--glob P` becomes `--path P`.
pub fn args(cmd: &str, args: &[String]) -> Vec<String> {
    let (mut out, mut paths, mut it) = (Vec::new(), Vec::new(), args.iter());
    while let Some(a) = it.next() {
        if a == "--" {
            out.push(a.clone());
            out.extend(it.by_ref().cloned());
            break;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next().cloned()).unwrap_or_default();
        match (cmd, name) {
            ("find" | "grep" | "refs", "--path") => paths.push(value()),
            ("find" | "grep" | "refs", "--limit") => out.extend(["--max".to_string(), value()]),
            ("find-symbol", "--max") => out.extend(["--limit".to_string(), value()]),
            ("find-symbol", "--glob" | "-g") => out.extend(["--path".to_string(), value()]),
            ("outline", "--path") => out.push(value()),
            _ => out.push(a.clone()),
        }
    }
    match out.iter().position(|a| a == "--") {
        Some(_) => out.extend(paths),
        None if paths.is_empty() => {}
        None => {
            out.push("--".to_string());
            let flags_end = out.len() - 1;
            let positional: Vec<String> = out.drain(..flags_end).collect();
            let (flags, words) = split_flags(cmd, positional);
            out = flags.into_iter().chain(["--".to_string()]).chain(words).chain(paths).collect();
        }
    }
    out
}

// Splits arguments into flags (with the values of those that take one) and positional words, so paths can follow the words after `--`.
fn split_flags(cmd: &str, args: Vec<String>) -> (Vec<String>, Vec<String>) {
    let spec = match cmd {
        "refs" => super::refs::SPEC,
        _ => super::find::SPEC,
    };
    let takes_value = |a: &str| {
        let name = a.trim_start_matches('-');
        spec.iter().any(|f| f.value && (f.long == name || (a.len() == 2 && name.starts_with(f.short))))
    };
    let (mut flags, mut words, mut it) = (Vec::new(), Vec::new(), args.into_iter());
    while let Some(a) = it.next() {
        if a.starts_with('-') && a.len() > 1 {
            let more = takes_value(&a) && !a.contains('=');
            flags.push(a);
            if more {
                flags.extend(it.next());
            }
        } else {
            words.push(a);
        }
    }
    (flags, words)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Turns words into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks each command's other spellings become its own, the rest pass through, and `--` keeps the pattern apart from the paths.
    #[test]
    fn test_align() {
        assert_eq!(
            args("find", &v(&["x", "--path", "src", "--limit", "5", "-i"])),
            v(&["--max", "5", "-i", "--", "x", "src"])
        );
        assert_eq!(
            args("refs", &v(&["--path=api", "Run", "-g", "*.cs"])),
            v(&["-g", "*.cs", "--", "Run", "api"])
        );
        assert_eq!(
            args("find", &v(&["-m", "3", "--", "-x", "--path", "a"])),
            v(&["-m", "3", "--", "-x", "--path", "a"])
        );
        assert_eq!(args("find", &v(&["--path", "a", "--", "pat"])), v(&["--", "pat", "a"]));
        assert_eq!(
            args("find-symbol", &v(&["A", "--max=4", "-g", "src", "--glob", "**/x"])),
            v(&["A", "--limit", "4", "--path", "src", "--path", "**/x"])
        );
        assert_eq!(args("outline", &v(&["--path", "a.rs", "-d", "2"])), v(&["a.rs", "-d", "2"]));
        assert_eq!(args("ls", &v(&["--path", "a"])), v(&["--path", "a"]));
        assert_eq!(args("find", &v(&["x", "a"])), v(&["x", "a"]));
    }
}
