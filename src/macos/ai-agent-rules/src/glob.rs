// Matches the `paths:` globs of a rule file against a path: `*` and `?` within a segment, `**` across segments and `{a,b}` alternatives.

// Expands the `{a,b}` alternatives of a glob into plain globs.
fn expand(glob: &str) -> Vec<String> {
    let Some(open) = glob.find('{') else {
        return vec![glob.to_string()];
    };
    let Some(close) = glob[open..].find('}').map(|c| open + c) else {
        return vec![glob.to_string()];
    };
    let (head, tail) = (&glob[..open], &glob[close + 1..]);
    glob[open + 1..close].split(',').flat_map(|alt| expand(&format!("{head}{alt}{tail}"))).collect()
}

// Matches one path segment against one glob segment.
fn segment(p: &[char], s: &[char]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some('*'), _) => segment(&p[1..], s) || (!s.is_empty() && segment(p, &s[1..])),
        (Some('?'), Some(_)) => segment(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => segment(&p[1..], &s[1..]),
        _ => false,
    }
}

// Matches path segments against glob segments; with `prefix`, a path that runs out first matches, since the folder may hold matching files.
fn segments(p: &[&str], s: &[&str], prefix: bool) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(&"**"), _) => segments(&p[1..], s, prefix) || (!s.is_empty() && segments(p, &s[1..], prefix)),
        (Some(_), None) => prefix,
        (None, Some(_)) => false,
        (Some(a), Some(b)) => {
            let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
            segment(&a, &b) && segments(&p[1..], &s[1..], prefix)
        }
    }
}

// Tells whether a `/`-separated relative path matches a glob; a folder (`prefix`) matches when files under it could.
pub fn matches(glob: &str, path: &str, prefix: bool) -> bool {
    let parts =
        |t: &str| t.split('/').filter(|x| !x.is_empty() && *x != ".").map(str::to_string).collect::<Vec<_>>();
    let s = parts(path);
    let s: Vec<&str> = s.iter().map(String::as_str).collect();
    expand(glob).iter().any(|g| {
        let p = parts(g);
        segments(&p.iter().map(String::as_str).collect::<Vec<_>>(), &s, prefix)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Files match by segment, with stars, question marks, double stars and alternatives.
    #[test]
    fn test_matches_files() {
        assert!(matches("src/**/*.ts", "src/a/b/c.ts", false));
        assert!(matches("src/**/*.ts", "src/c.ts", false));
        assert!(!matches("src/**/*.ts", "lib/c.ts", false));
        assert!(matches("*.md", "README.md", false));
        assert!(!matches("*.md", "docs/README.md", false));
        assert!(matches("./docs/?.md", "docs/a.md", false));
        assert!(!matches("docs/?.md", "docs/ab.md", false));
        assert!(matches("src/*.{ts,tsx}", "src/a.tsx", false));
        assert!(!matches("src/*.{ts,tsx}", "src/a.js", false));
        assert!(matches("src/{a", "src/{a", false));
        assert!(!matches("src", "src/a.ts", false));
        assert!(matches("**", "any/thing", false));
    }

    // A folder matches when a file under it could, and the top folder matches everything.
    #[test]
    fn test_matches_folders() {
        assert!(matches("src/**/*.ts", "src", true));
        assert!(matches("src/api/*.ts", "src", true));
        assert!(!matches("src/api/*.ts", "lib", true));
        assert!(matches("docs/**", "", true));
        assert!(!matches("docs/**", "", false));
    }
}
