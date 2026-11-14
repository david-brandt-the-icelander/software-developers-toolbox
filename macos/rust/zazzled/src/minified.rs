// Terse view of minified JSON and XML: reformats the long lines into one value or element per line, runs data.rs's or xml.rs's view over the result, and maps its line numbers back to the source lines, so a one-line file still loses its long lists, repeated elements and long values.
use crate::data;
use crate::xml;

const LONG_LINE: usize = 500;

// One reformatted line and the source line (1-based) its first token is on.
type Pretty = Vec<(String, usize)>;

// True when most of the text sits on lines of 500+ characters, as minifiers and serializers write it.
pub fn is_minified(text: &str) -> bool {
    let long: usize = text.lines().map(str::len).filter(|&n| n >= LONG_LINE).sum();
    long > 0 && long * 2 >= text.len()
}

// Returns the view of a minified JSON file, where any 10+ sibling items or members collapse (as in a lock file), or None when it does not parse or nothing would be shortened.
pub fn json_view(shown: &str, text: &str, lock: bool) -> Option<String> {
    let pretty = pretty_json(text)?;
    let joined: String = pretty.iter().map(|(l, _)| format!("{l}\n")).collect();
    let view = if lock {
        data::view(shown, &joined, data::Kind::Json, true)
    } else {
        data::view_generated(shown, &joined)
    };
    remap(shown, text, &pretty, &view?)
}

// Returns the view of a minified XML file, or None when it does not parse or nothing would be shortened.
pub fn xml_view(shown: &str, text: &str) -> Option<String> {
    let pretty = pretty_xml(text)?;
    let joined: String = pretty.iter().map(|(l, _)| format!("{l}\n")).collect();
    remap(shown, text, &pretty, &xml::view(shown, &joined)?)
}

// Rewrites a view of the reformatted text with source line numbers (`N~` for a kept line, `A-B` for a note) and the source's line count; None unless it is shorter than the source.
fn remap(shown: &str, text: &str, pretty: &Pretty, view: &str) -> Option<String> {
    let (head, body) = view.split_once('\n').unwrap_or((view, ""));
    let steps = head.split_once("terse: ")?.1;
    let at = |n: &str| n.parse::<usize>().ok().and_then(|n| pretty.get(n.wrapping_sub(1))).map(|p| p.1);
    let mut lines = Vec::new();
    for line in body.lines() {
        let (key, rest) = line.split_once('\t')?;
        let key = match key.trim_end_matches('~').split_once('-') {
            Some((a, b)) => format!("{}-{}", at(a)?, at(b)?),
            None => format!("{}~", at(key.trim_end_matches('~'))?),
        };
        lines.push(format!("{key}\t{rest}"));
    }
    let minified = text.lines().filter(|l| l.len() >= LONG_LINE).count();
    let out = format!(
        "{shown} {} lines; terse: {minified} minified lines reformatted, {steps}\n{}",
        text.lines().count(),
        lines.join("\n")
    );
    (out.len() < text.len()).then_some(out)
}

// Returns the 1-based line of a byte offset.
fn line_at(starts: &[usize], at: usize) -> usize {
    starts.partition_point(|&s| s <= at)
}

// Returns the byte offsets where the text's lines start.
fn starts(text: &str) -> Vec<usize> {
    std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect()
}

// Reformats JSON as `serde_json::to_string_pretty` would (two spaces, one value per line, `{}` and `[]` inline), keeping strings and numbers as written; None when brackets do not match or a token is not JSON.
fn pretty_json(text: &str) -> Option<Pretty> {
    let (b, starts) = (text.as_bytes(), starts(text));
    let (mut out, mut cur, mut at, mut stack, mut i) = (Vec::new(), String::new(), 0, Vec::new(), 0);
    let skip = |mut j: usize| {
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        j
    };
    let flush = |cur: &mut String, at: usize, out: &mut Pretty| {
        if !cur.trim().is_empty() {
            out.push((std::mem::take(cur), at));
        }
        cur.clear();
    };
    loop {
        i = skip(i);
        let Some(&c) = b.get(i) else { break };
        if matches!(c, b'}' | b']') {
            if stack.pop() != Some(c) {
                return None;
            }
            flush(&mut cur, at, &mut out);
            cur = format!("{}{}", "  ".repeat(stack.len()), c as char);
            at = line_at(&starts, i);
            i += 1;
            continue;
        }
        if cur.is_empty() {
            cur = "  ".repeat(stack.len());
            at = line_at(&starts, i);
        }
        match c {
            b'{' | b'[' => {
                let close = if c == b'{' { b'}' } else { b']' };
                let j = skip(i + 1);
                cur.push(c as char);
                if b.get(j) == Some(&close) {
                    cur.push(close as char);
                    i = j + 1;
                } else {
                    stack.push(close);
                    flush(&mut cur, at, &mut out);
                    i += 1;
                }
            }
            b',' => {
                cur.push(',');
                flush(&mut cur, at, &mut out);
                i += 1;
            }
            b':' => {
                cur.push_str(": ");
                i += 1;
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                cur.push_str(text.get(i..=j)?);
                i = j + 1;
            }
            _ => {
                let n = b[i..].iter().take_while(|c| c.is_ascii_alphanumeric() || b"+-.".contains(c)).count();
                if n == 0 {
                    return None;
                }
                cur.push_str(&text[i..i + n]);
                i += n;
            }
        }
    }
    flush(&mut cur, at, &mut out);
    (stack.is_empty() && !out.is_empty()).then_some(out)
}

// The XML tokens the reformatter tells apart.
#[derive(PartialEq, Debug)]
enum Tok<'t> {
    Open(&'t str),
    Close(&'t str),
    Other,
    Text,
}

// Splits XML into tokens with their raw text and source line: tags, comments, CDATA, declarations and trimmed text; None when a markup token never ends.
fn xml_tokens<'t>(text: &'t str) -> Option<Vec<(Tok<'t>, &'t str, usize)>> {
    let starts = starts(text);
    let (mut toks, mut i) = (Vec::new(), 0);
    while i < text.len() {
        let rest = &text[i..];
        if !rest.starts_with('<') {
            let n = rest.find('<').unwrap_or(rest.len());
            let t = rest[..n].trim();
            if !t.is_empty() {
                toks.push((Tok::Text, t, line_at(&starts, i + rest[..n].find(t).unwrap_or(0))));
            }
            i += n;
            continue;
        }
        let end = if rest.starts_with("<!--") {
            rest.find("-->")? + 3
        } else if rest.starts_with("<![CDATA[") {
            rest.find("]]>")? + 3
        } else {
            tag_end(rest)?
        };
        let raw = &rest[..end];
        let name = |s: &'t str| -> &'t str {
            s.split(|c: char| c.is_whitespace() || c == '/' || c == '>').next().unwrap_or("")
        };
        let tok = if let Some(closing) = raw.strip_prefix("</") {
            Tok::Close(name(closing))
        } else if raw.starts_with("<!") || raw.starts_with("<?") || raw.ends_with("/>") {
            Tok::Other
        } else {
            Tok::Open(name(&raw[1..]))
        };
        toks.push((tok, raw, line_at(&starts, i)));
        i += end;
    }
    Some(toks)
}

// Returns the length of a tag at the start of the text, through its `>`, skipping quoted attribute values and a DOCTYPE's `[..]` subset.
fn tag_end(rest: &str) -> Option<usize> {
    let (mut quote, mut square) = (None, 0);
    for (i, c) in rest.char_indices().skip(1) {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[') => square += 1,
            (None, ']') => square -= 1,
            (None, '>') if square <= 0 => return Some(i + 1),
            _ => {}
        }
    }
    None
}

// Reformats XML with one element per line indented by two spaces, an element holding only text on one line; None when tags do not match.
fn pretty_xml(text: &str) -> Option<Pretty> {
    let toks = xml_tokens(text)?;
    let (mut out, mut stack, mut k) = (Vec::new(), Vec::new(), 0);
    let one = |s: &str| s.replace(['\r', '\n'], " ");
    while k < toks.len() {
        let (tok, raw, line) = &toks[k];
        let pad = "  ".repeat(stack.len());
        match tok {
            Tok::Open(name) => {
                let inline = match (toks.get(k + 1), toks.get(k + 2)) {
                    (Some((Tok::Close(c), r, _)), _) if c == name => Some((String::new(), *r, 2)),
                    (Some((Tok::Text, t, _)), Some((Tok::Close(c), r, _))) if c == name => {
                        Some((one(t), *r, 3))
                    }
                    _ => None,
                };
                if let Some((body, close, n)) = inline {
                    out.push((format!("{pad}{}{body}{close}", one(raw)), *line));
                    k += n;
                    continue;
                }
                out.push((format!("{pad}{}", one(raw)), *line));
                stack.push(*name);
            }
            Tok::Close(name) => {
                if stack.pop() != Some(*name) {
                    return None;
                }
                out.push((format!("{}{raw}", "  ".repeat(stack.len())), *line));
            }
            Tok::Other | Tok::Text => out.push((format!("{pad}{}", one(raw)), *line)),
        }
        k += 1;
    }
    (stack.is_empty() && !out.is_empty()).then_some(out)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks a one-line JSON array of records reads as its first and last records, a long string is cut, and line numbers point at line 1.
    #[test]
    fn test_json_view() {
        let rows: Vec<String> = (1..=40)
            .map(|n| format!("{{\"id\":{n},\"name\":\"user {n}\",\"tags\":[],\"ok\":true}}"))
            .collect();
        let text =
            format!("{{\"total\":40,\"rows\":[{}],\"blob\":\"{}\"}}\n", rows.join(","), "A".repeat(130));
        assert!(is_minified(&text));
        let v = json_view("a.json", &text, false).expect("view");
        let lines: Vec<&str> = v.lines().collect();
        assert!(lines[0].starts_with("a.json 1 lines; terse: 1 minified lines reformatted, "), "{v}");
        assert_eq!(lines[1..4], ["1~\t{", "1~\t  \"total\": 40,", "1~\t  \"rows\": ["]);
        assert!(v.contains("\n1~\t      \"id\": 1,\n") && v.contains("\n1~\t      \"id\": 40,\n"), "{v}");
        assert!(v.contains("\n1-1\t⋯ 38 item(s)\n") && v.contains("\"blob\": \"AAAA"), "{v}");
        assert!(!v.contains("\"id\": 2,"));
        let members: Vec<String> = (1..=30).map(|n| format!("\"member_number_{n}\":{{\"v\":{n}}}")).collect();
        let text = format!("{{{}}}", members.join(","));
        let v = json_view("b.json", &format!("{text}{}", " ".repeat(text.len())), false).expect("view");
        assert!(v.contains("\n1-1\t⋯ 28 member(s): \"member_number_2\", \"member_number_3\", \"member_number_4\", \"member_number_5\", …\n"), "{v}");
    }

    // Checks the JSON reformatter: nesting, empty containers, escapes, numbers, line numbers across lines, and what it rejects.
    #[test]
    fn test_pretty_json() {
        let p = pretty_json("{\"a\":[1,-2.5e3,{}],\n\"b\":\"x\\\"y\",\"c\":[ ],\"d\":null}").expect("pretty");
        let lines: Vec<(&str, usize)> = p.iter().map(|(l, n)| (l.as_str(), *n)).collect();
        assert_eq!(
            lines,
            [
                ("{", 1),
                ("  \"a\": [", 1),
                ("    1,", 1),
                ("    -2.5e3,", 1),
                ("    {}", 1),
                ("  ],", 1),
                ("  \"b\": \"x\\\"y\",", 2),
                ("  \"c\": [],", 2),
                ("  \"d\": null", 2),
                ("}", 2)
            ]
        );
        assert!(
            pretty_json("[1}").is_none() && pretty_json("[1").is_none() && pretty_json("[1,/2]").is_none()
        );
        assert!(pretty_json("  ").is_none() && pretty_json("\"open").is_none());
    }

    // Checks a one-line XML document reads with its repeated elements collapsed, and the reformatter's tokens and errors.
    #[test]
    fn test_xml_view() {
        let items: String =
            (1..=12).map(|n| format!("<item id=\"{n}\"><name>n{n}</name><v/></item>")).collect();
        let text = format!("<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY e \"x>\">]><r a='>'><!-- c --><list>{items}</list><empty></empty><t> hi\nthere </t><![CDATA[<x>]]></r>");
        let v = xml_view("a.xml", &text).expect("view");
        assert!(v.starts_with("a.xml 2 lines; terse: 1 minified lines reformatted, 1 elements\n1~\t<?xml version=\"1.0\"?>\n"), "{v}");
        assert!(v.contains("\n1~\t    <item id=\"1\">\n1~\t      <name>n1</name>\n1~\t      <v/>\n1~\t    </item>\n1-1\t⋯ 10 <item> element(s)\n"), "{v}");
        assert!(
            v.contains("\n1~\t  <empty></empty>\n1~\t  <t>hi there</t>\n2~\t  <![CDATA[<x>]]>\n2~\t</r>"),
            "{v}"
        );
        assert!(
            pretty_xml("<a><b></a>").is_none() && pretty_xml("<a>").is_none() && pretty_xml("<a").is_none()
        );
        assert!(xml_tokens("<!-- open").is_none() && xml_tokens("<![CDATA[ open").is_none());
        assert!(!is_minified("{\n  \"a\": 1\n}\n") && !is_minified(""));
        assert!(xml_view("a.xml", "<a>x</a>").is_none() && json_view("a.json", "[1]", false).is_none());
    }
}
