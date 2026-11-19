// The Rebase view's model: reading and changing the command word on each line of git's rebase todo list.

// The commands a key can set, with their keys.
const COMMANDS: [(char, &str); 6] =
    [('p', "pick"), ('r', "reword"), ('e', "edit"), ('s', "squash"), ('f', "fixup"), ('d', "drop")];

// Commands that act on a commit and so can be switched to another command.
const COMMIT_COMMANDS: [&str; 12] =
    ["pick", "p", "reword", "r", "edit", "e", "squash", "s", "fixup", "f", "drop", "d"];

// Returns the command for a key, if the key sets one.
pub fn command_for_key(c: char) -> Option<&'static str> {
    COMMANDS.iter().find(|(k, _)| *k == c).map(|(_, name)| *name)
}

// Splits a todo line into its command word and the rest.
fn split(line: &str) -> (&str, &str) {
    let line = line.trim_start();
    match line.find(' ') {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, ""),
    }
}

// Returns the line with its command replaced, or None if the line is not a commit command.
pub fn set_command(line: &str, command: &str) -> Option<String> {
    let (cmd, rest) = split(line);
    if !COMMIT_COMMANDS.contains(&cmd) {
        return None;
    }
    Some(format!("{command} {rest}"))
}

// Returns the full name of a command word (`s` becomes `squash`).
fn full(cmd: &str) -> &str {
    COMMANDS
        .iter()
        .find(|(k, name)| cmd.len() == 1 && cmd.starts_with(*k) || *name == cmd)
        .map_or(cmd, |(_, n)| n)
}

// Returns the row text shown in the Rebase view: the command padded to a column, then the commit and subject.
pub fn row(line: &str) -> String {
    if line.trim().is_empty() {
        return String::new();
    }
    let (cmd, rest) = split(line);
    format!("{:<7}{rest}", full(cmd))
}

// Returns a warning for todo lists git would reject: a squash or fixup with no commit before it.
pub fn warning(lines: &[String]) -> Option<String> {
    let first = lines
        .iter()
        .map(|l| split(l).0)
        .find(|c| COMMIT_COMMANDS.contains(c) && !matches!(*c, "drop" | "d"))?;
    match full(first) {
        "squash" | "fixup" => Some(format!("The first commit cannot be a {}", full(first))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks keys, command replacement and lines that are not commit commands.
    #[test]
    fn test_set_command() {
        assert_eq!(command_for_key('s'), Some("squash"));
        assert_eq!(command_for_key('x'), None);
        assert_eq!(set_command("pick 1a2b Add parser", "fixup").as_deref(), Some("fixup 1a2b Add parser"));
        assert_eq!(set_command("s 1a2b x", "pick").as_deref(), Some("pick 1a2b x"));
        assert_eq!(set_command("exec make", "pick"), None);
        assert_eq!(set_command("", "pick"), None);
    }

    // Checks the displayed rows.
    #[test]
    fn test_row() {
        assert_eq!(row("p 1a2b Add parser"), "pick   1a2b Add parser");
        assert_eq!(row("squash 1a2b x"), "squash 1a2b x");
        assert_eq!(row("break"), "break  ");
        assert_eq!(row("  "), "");
    }

    // Checks the warning for a squash or fixup at the top.
    #[test]
    fn test_warning() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(warning(&l(&["pick a x", "squash b y"])), None);
        assert!(warning(&l(&["drop a x", "f b y"])).is_some_and(|w| w.contains("fixup")));
        assert!(warning(&l(&["squash a x"])).is_some());
        assert_eq!(warning(&l(&["exec make"])), None);
    }
}
