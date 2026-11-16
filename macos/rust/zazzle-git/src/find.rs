// Plain-text search over lines: next match after a position (wrapping) and replace-all, ignoring case unless the query has a capital.

use crate::buffer::Pos;

// Returns true if the query should match case exactly (it contains a capital letter).
fn exact_case(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

// Folds a character for comparison.
fn fold(c: char, exact: bool) -> char {
    if exact {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

// Returns the columns where the query starts in the line.
fn matches_in(line: &[char], query: &[char], exact: bool) -> Vec<usize> {
    if query.is_empty() || query.len() > line.len() {
        return Vec::new();
    }
    (0..=line.len() - query.len())
        .filter(|&c| {
            line[c..c + query.len()].iter().zip(query).all(|(a, b)| fold(*a, exact) == fold(*b, exact))
        })
        .collect()
}

// Finds the first match at or after `from`, wrapping to the top; returns its start and end.
pub fn find_next(lines: &[Vec<char>], query: &str, from: Pos) -> Option<(Pos, Pos)> {
    let q: Vec<char> = query.chars().collect();
    let exact = exact_case(query);
    let n = lines.len();
    for step in 0..=n {
        let row = (from.row + step) % n;
        let min = if step == 0 { from.col } else { 0 };
        let hits = matches_in(&lines[row], &q, exact);
        let hit = if step == n {
            hits.first().filter(|c| **c < from.col)
        } else {
            hits.iter().find(|c| **c >= min)
        };
        if let Some(&col) = hit {
            return Some((Pos { row, col }, Pos { row, col: col + q.len() }));
        }
    }
    None
}

// Replaces every match in the lines and returns how many were replaced.
pub fn replace_all(lines: &mut [Vec<char>], query: &str, with: &str) -> usize {
    let q: Vec<char> = query.chars().collect();
    let w: Vec<char> = with.chars().collect();
    let exact = exact_case(query);
    let mut count = 0;
    for line in lines.iter_mut() {
        let mut out = Vec::with_capacity(line.len());
        let mut c = 0;
        let hits = matches_in(line, &q, exact);
        for &h in &hits {
            if h < c {
                continue;
            }
            out.extend_from_slice(&line[c..h]);
            out.extend_from_slice(&w);
            c = h + q.len();
            count += 1;
        }
        out.extend_from_slice(&line[c..]);
        *line = out;
    }
    count
}

// Returns the index of the first row at or after `from` (wrapping) whose text contains the query.
pub fn find_row(rows: &[String], query: &str, from: usize) -> Option<usize> {
    let q: Vec<char> = query.chars().collect();
    let exact = exact_case(query);
    let n = rows.len();
    (0..n)
        .map(|s| (from + s) % n)
        .find(|&r| !matches_in(&rows[r].chars().collect::<Vec<_>>(), &q, exact).is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Builds lines of characters from strings.
    fn lines(v: &[&str]) -> Vec<Vec<char>> {
        v.iter().map(|s| s.chars().collect()).collect()
    }

    // Checks forward search, smart case and wrapping to the top.
    #[test]
    fn test_find_next() {
        let l = lines(&["Foo bar", "baz foo"]);
        let p = |row, col| Pos { row, col };
        assert_eq!(find_next(&l, "foo", p(0, 0)), Some((p(0, 0), p(0, 3))));
        assert_eq!(find_next(&l, "foo", p(0, 1)), Some((p(1, 4), p(1, 7))));
        assert_eq!(find_next(&l, "Foo", p(0, 1)), Some((p(0, 0), p(0, 3))));
        assert_eq!(find_next(&l, "foo", p(1, 5)), Some((p(0, 0), p(0, 3))));
        assert_eq!(find_next(&l, "zzz", p(0, 0)), None);
        assert_eq!(find_next(&l, "", p(0, 0)), None);
        let one = lines(&["ab ab"]);
        assert_eq!(find_next(&one, "ab", p(0, 4)), Some((p(0, 0), p(0, 2))));
    }

    // Checks replacing every match, including overlapping candidates.
    #[test]
    fn test_replace_all() {
        let mut l = lines(&["aaa a", "A"]);
        assert_eq!(replace_all(&mut l, "aa", "b"), 1);
        assert_eq!(l, lines(&["ba a", "A"]));
        assert_eq!(replace_all(&mut l, "a", "x"), 3);
        assert_eq!(l, lines(&["bx x", "x"]));
    }

    // Checks finding a row in a list.
    #[test]
    fn test_find_row() {
        let rows = vec!["src/a.rs".to_string(), "README".to_string(), "src/b.rs".to_string()];
        assert_eq!(find_row(&rows, "src", 1), Some(2));
        assert_eq!(find_row(&rows, "readme", 2), Some(1));
        assert_eq!(find_row(&rows, "nope", 0), None);
    }
}
