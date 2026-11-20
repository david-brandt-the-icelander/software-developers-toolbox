// Reads purpose markers in a rule or skill file and decides which parts of it match the wanted purposes.

// Text that starts every purpose marker.
const MARK: &str = "#zazzle-purpose";

// What part of a file matches the wanted purposes.
#[derive(Debug, PartialEq)]
pub enum Part {
    Whole,
    Lines(Vec<(usize, usize)>),
    Nothing,
}

// The three kinds of marker.
#[derive(PartialEq)]
enum Kind {
    File,
    Begin,
    End,
}

// One marker found on a line, with the purpose it names.
struct Marker<'a> {
    kind: Kind,
    purpose: &'a str,
}

// Text allowed before a marker on its line, besides whitespace: the opener of a comment.
const OPENERS: [&str; 7] = ["<!--", "//", "/*", "--", "#", ";", "%"];

// Parses the marker on a line, if any; it must start the line or follow a comment opener, and the purpose is its run of letters, digits, `_`, `.` and `-`.
fn marker(line: &str) -> Option<Marker<'_>> {
    let at = line.find(MARK)?;
    let before = line[..at].trim();
    if !before.is_empty() && !OPENERS.contains(&before) {
        return None;
    }
    let rest = &line[at + MARK.len()..];
    let (kind, rest) = if let Some(r) = rest.strip_prefix("-begin:") {
        (Kind::Begin, r)
    } else if let Some(r) = rest.strip_prefix("-end:") {
        (Kind::End, r)
    } else {
        (Kind::File, rest.strip_prefix(':')?)
    };
    let end = rest.find(|c: char| !(c.is_alphanumeric() || "_.-".contains(c))).unwrap_or(rest.len());
    let purpose = rest[..end].trim_end_matches('-');
    (!purpose.is_empty()).then_some(Marker { kind, purpose })
}

// Tells whether a line is a purpose marker, so segment output can leave it out.
pub fn is_marker(line: &str) -> bool {
    marker(line).is_some()
}

// Tells whether a purpose name starts with a letter or digit and can be written in a marker, so a --purpose value that could never match (or is a flag) is refused.
pub fn valid(name: &str) -> bool {
    name.starts_with(char::is_alphanumeric)
        && marker(&format!("{MARK}:{name}")).is_some_and(|m| m.purpose == name)
}

// Sorts line ranges and joins those that overlap or touch.
fn merge(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (a, b) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

// Decides which part of a file's text applies: all of it if it has no markers or a file-level marker for a wanted purpose, else the wanted segments, else nothing.
pub fn part(text: &str, wanted: &[String]) -> Part {
    let mut any = false;
    let mut ranges = Vec::new();
    let mut open: Vec<(&str, usize)> = Vec::new();
    let mut total = 0;
    for (i, line) in text.lines().enumerate() {
        total = i + 1;
        let Some(m) = marker(line) else { continue };
        any = true;
        if !wanted.iter().any(|w| w == m.purpose) {
            continue;
        }
        let at = open.iter().position(|o| o.0 == m.purpose);
        match (m.kind, at) {
            (Kind::File, _) => return Part::Whole,
            (Kind::Begin, None) => open.push((m.purpose, i + 2)),
            (Kind::End, Some(p)) => {
                let start = open.remove(p).1;
                if start <= i {
                    ranges.push((start, i));
                }
            }
            _ => {}
        }
    }
    ranges.extend(open.iter().filter(|o| o.1 <= total).map(|o| (o.1, total)));
    if !any {
        return Part::Whole;
    }
    let ranges = merge(ranges);
    if ranges.is_empty() {
        Part::Nothing
    } else {
        Part::Lines(ranges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Wanted purposes from literals.
    fn want(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // A file with no markers applies whole.
    #[test]
    fn test_part_unmarked_is_whole() {
        assert_eq!(part("a\nb\n", &want(&["x"])), Part::Whole);
    }

    // A file-level marker for a wanted purpose returns the whole file, and one for another purpose returns nothing.
    #[test]
    fn test_part_file_level() {
        let text = "<!-- #zazzle-purpose:commit -->\nrule\n";
        assert_eq!(part(text, &want(&["commit"])), Part::Whole);
        assert_eq!(part(text, &want(&["test", "commit"])), Part::Whole);
        assert_eq!(part(text, &want(&["test"])), Part::Nothing);
    }

    // Segments return the lines between their markers, merged across purposes.
    #[test]
    fn test_part_segments_merge() {
        let text = "intro\n#zazzle-purpose-begin:a\n1\n2\n#zazzle-purpose-end:a\n#zazzle-purpose-begin:b\n3\n#zazzle-purpose-end:b\n#zazzle-purpose-begin:c\n4\n#zazzle-purpose-end:c\n";
        assert_eq!(part(text, &want(&["a"])), Part::Lines(vec![(3, 4)]));
        assert_eq!(part(text, &want(&["a", "b"])), Part::Lines(vec![(3, 4), (7, 7)]));
        assert_eq!(part(text, &want(&["z"])), Part::Nothing);
    }

    // A file-level and a segment marker for the same purpose return the whole file.
    #[test]
    fn test_part_file_and_segment_is_whole() {
        let text = "#zazzle-purpose-begin:a\n1\n#zazzle-purpose-end:a\n#zazzle-purpose:a\n";
        assert_eq!(part(text, &want(&["a"])), Part::Whole);
    }

    // A segment without an end runs to the end of the file; stray ends, repeated begins and empty segments are ignored.
    #[test]
    fn test_part_odd_markers() {
        assert_eq!(part("#zazzle-purpose-begin:a\n1\n2\n", &want(&["a"])), Part::Lines(vec![(2, 3)]));
        assert_eq!(part("#zazzle-purpose-end:a\n1\n", &want(&["a"])), Part::Nothing);
        assert_eq!(
            part(
                "#zazzle-purpose-begin:a\n#zazzle-purpose-begin:a\n1\n#zazzle-purpose-end:a\n",
                &want(&["a"])
            ),
            Part::Lines(vec![(2, 3)])
        );
        assert_eq!(part("#zazzle-purpose-begin:a\n#zazzle-purpose-end:a\n", &want(&["a"])), Part::Nothing);
        assert_eq!(part("#zazzle-purpose-begin:a", &want(&["a"])), Part::Nothing);
    }

    // Text that merely looks like a marker is not one.
    #[test]
    fn test_marker_rejects_lookalikes() {
        assert!(marker("#zazzle-purpose").is_none());
        assert!(marker("#zazzle-purpose:").is_none());
        assert!(marker("#zazzle-purposeX:a").is_none());
        assert_eq!(marker("/* #zazzle-purpose:a*/").map(|m| m.purpose), Some("a"));
        assert_eq!(marker("<!-- #zazzle-purpose:a-b-->").map(|m| m.purpose), Some("a-b"));
        assert_eq!(marker("  #zazzle-purpose-end:a.b_c").map(|m| m.purpose), Some("a.b_c"));
    }

    // Marker text quoted in prose or code spans is not a marker, and is_marker agrees.
    #[test]
    fn test_marker_ignores_prose() {
        assert!(
            marker("- keep marked `#zazzle-purpose:NAME` or between `#zazzle-purpose-begin:NAME`").is_none()
        );
        assert!(!is_marker("see #zazzle-purpose:a here"));
        assert!(is_marker("#zazzle-purpose:a"));
    }

    // Only names a marker can carry are valid purposes.
    #[test]
    fn test_valid_names() {
        for good in ["commit", "a.b_c-d"] {
            assert!(valid(good), "{good}");
        }
        for bad in ["-x", "commit-", "a b", "", "a:b"] {
            assert!(!valid(bad), "{bad}");
        }
    }
}
