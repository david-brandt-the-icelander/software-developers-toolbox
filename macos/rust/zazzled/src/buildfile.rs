// Terse view of CMake files and Makefiles, line by line: collapses comments of 3+ lines (and CMake bracket comments), runs of 3+ includes and lists of 6+ one-word items to their first and last, hides CMake `message(STATUS ..)`-style and Make `$(info ..)` logging, and cuts long values, keeping source line numbers.
use crate::data::{shorten_strings, shorten_words};
use crate::markup::Plan;
use std::path::Path;

const LONG_COMMENT: usize = 3;
const LONG_INCLUDES: usize = 3;
const LONG_LIST: usize = 6;
// CMake commands whose argument lists stay whole: signatures and the command lines they run.
const KEEP_ARGS: [&str; 6] =
    ["function", "macro", "add_custom_command", "add_custom_target", "execute_process", "add_test"];
const LOG_MODES: [&str; 7] =
    ["status", "verbose", "debug", "trace", "check_start", "check_pass", "check_fail"];

// The file kinds the view handles.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    CMake,
    Make,
}

// Returns the kind of a path: CMake for CMakeLists.txt and .cmake, Make for Makefile, makefile, GNUmakefile, Makefile.* and .mk or .mak.
pub fn kind_of(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e);
    if name == "cmakelists.txt" || ext == Some("cmake") {
        return Some(Kind::CMake);
    }
    let make = matches!(name.as_str(), "makefile" | "gnumakefile") || name.starts_with("makefile.");
    (make || matches!(ext, Some("mk" | "mak"))).then_some(Kind::Make)
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, text: &str, kind: Kind) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut i = 0;
    while i < src.len() {
        if let Some(end) = comment_end(&src, i, kind) {
            if end + 1 - i >= LONG_COMMENT {
                plan.hide(i, end, format!("comment, {} line(s)", end - i + 1), "comments");
            } else {
                (i..=end).for_each(|l| plan.keep(l, src[l], false));
            }
            i = end + 1;
            continue;
        }
        if let Some(end) = log_end(&src, i, kind) {
            plan.count("logging");
            i = end + 1;
            continue;
        }
        let n = src[i..].iter().take_while(|l| is_include(l, kind)).count();
        if n >= LONG_INCLUDES {
            plan.hide(i, i + n - 1, format!("{n} include(s)"), "includes");
            i += n;
            continue;
        }
        keep(&mut plan, src[i], i);
        if let Some(items) = list(&src, i, kind).filter(|v| v.len() >= LONG_LIST) {
            let (first, last) = (items[0], items[items.len() - 1]);
            (i + 1..=first).for_each(|l| keep(&mut plan, src[l], l));
            plan.hide(first + 1, last - 1, format!("{} item(s)", items.len() - 2), "lists");
            keep(&mut plan, src[last], last);
            i = last + 1;
            continue;
        }
        i += 1;
    }
    plan.steps.retain(|(_, n)| *n > 0);
    if plan.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), plan.lines.join("\n")))
}

// Keeps a line, cutting long strings and blobs.
fn keep(plan: &mut Plan, line: &str, i: usize) {
    let (s, a) = shorten_strings(line);
    let (s, b) = shorten_words(&s);
    plan.add("long values", a + b);
    plan.keep(i, &s, a + b > 0);
}

// Returns the `]==]` that closes a CMake bracket comment opened at the start of a trimmed line (`#[[`, `#[==[`).
fn bracket_close(t: &str) -> Option<String> {
    let rest = t.strip_prefix("#[")?;
    let eqs = rest.chars().take_while(|&c| c == '=').count();
    rest[eqs..].starts_with('[').then(|| format!("]{}]", "=".repeat(eqs)))
}

// Returns the last line of the comment that starts line i: a run of `#` lines, or a CMake bracket comment.
fn comment_end(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    let t = src[i].trim_start();
    if let Some(close) = bracket_close(t).filter(|_| kind == Kind::CMake) {
        let open = t.find('[').unwrap_or(0) + close.len();
        return (i..src.len()).find(|&j| {
            if j == i {
                t[open..].contains(&close)
            } else {
                src[j].contains(&close)
            }
        });
    }
    let line = |l: &&str| {
        l.trim_start().starts_with('#') && (kind == Kind::Make || bracket_close(l.trim_start()).is_none())
    };
    let n = src[i..].iter().take_while(|l| line(l)).count();
    (n > 0).then(|| i + n - 1)
}

// Returns the change in parenthesis depth over a line, outside double-quoted strings and `#` comments.
fn depth(line: &str) -> i32 {
    let (mut d, mut quoted, mut escaped) = (0, false, false);
    for c in line.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' if !quoted => break,
            '(' if !quoted => d += 1,
            ')' if !quoted => d -= 1,
            _ => {}
        }
    }
    d
}

// Returns the last line of a logging statement starting line i: CMake `message(STATUS|VERBOSE|DEBUG|TRACE|CHECK_* ..)`, the mode on the same or the next line, or a Make `$(info ..)` alone on its line.
fn log_end(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    let t = src[i].trim().to_ascii_lowercase();
    if kind == Kind::Make {
        return (t.starts_with("$(info ") && t.ends_with(')') && depth(&t) == 0).then_some(i);
    }
    let args = t.strip_prefix("message")?.trim_start().strip_prefix('(')?.trim_start();
    let next = src.get(i + 1).map(|l| l.trim().to_ascii_lowercase()).unwrap_or_default();
    let args = if args.is_empty() { next.as_str() } else { args };
    let mode = args.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next().unwrap_or("");
    if !LOG_MODES.contains(&mode) {
        return None;
    }
    let mut d = 0;
    (i..src.len()).find(|&j| {
        d += depth(src[j]);
        d <= 0
    })
}

// True for an include line: CMake `include(..)` closed on the line, Make `include`, `-include` or `sinclude`.
fn is_include(line: &str, kind: Kind) -> bool {
    let t = line.trim_start().to_ascii_lowercase();
    match kind {
        Kind::CMake => {
            t.strip_prefix("include").is_some_and(|r| r.trim_start().starts_with('(')) && depth(&t) == 0
        }
        Kind::Make => ["include ", "-include ", "sinclude "].iter().any(|w| line.starts_with(w)),
    }
}

// True for one word on a line: no spaces, parentheses or keyword (CMake `PRIVATE`, `PUBLIC`, ...), ignoring a Make line's trailing `\`.
fn is_word(line: &str) -> bool {
    let t = line.trim().trim_end_matches('\\').trim_end();
    let keyword = t.len() > 1 && t.chars().all(|c| c.is_ascii_uppercase() || c == '_');
    !t.is_empty()
        && !keyword
        && !t.starts_with('#')
        && !t.contains(|c: char| c.is_whitespace() || c == '(' || c == ')')
}

// Returns the lines of the one-word items that line i opens: the arguments of a CMake command left open on it (not a function or macro signature, or a command line it runs), or the continued lines of a Make line ending in `\`.
fn list(src: &[&str], i: usize, kind: Kind) -> Option<Vec<usize>> {
    let mut items = Vec::new();
    if kind == Kind::Make {
        let mut j = i;
        while src[j].trim_end().ends_with('\\') && j + 1 < src.len() && is_word(src[j + 1]) {
            j += 1;
            items.push(j);
        }
        return Some(items);
    }
    let t = src[i].trim_start().to_ascii_lowercase();
    let keeps = KEEP_ARGS.iter().any(|w| t.strip_prefix(w).is_some_and(|r| r.trim_start().starts_with('(')));
    if depth(src[i]) <= 0 || keeps {
        return None;
    }
    let mut k = None;
    for (j, line) in src.iter().enumerate().skip(i + 1) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') && bracket_close(t).is_none() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if !is_word(line) || *k.get_or_insert(indent) != indent {
            break;
        }
        items.push(j);
    }
    Some(items)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks a CMakeLists.txt: a bracket comment, includes, a source list, logging over one and two lines, a keyword ending a list, and what stays.
    #[test]
    fn test_view_cmake() {
        let files: String = (1..=8).map(|n| format!("  src/f{n}.cpp\n")).collect();
        let libs: String = (1..=7).map(|n| format!("    lib{n}\n")).collect();
        let src = format!("#[[\nLicence\n]]\ncmake_minimum_required(VERSION 3.20)\ninclude(A)\ninclude(B)\nINCLUDE (C)\nmessage(STATUS \"Building\")\nmessage( VERBOSE \"a\"\n  \"b\")\nmessage(WARNING \"careful\")\nadd_library(core\n{files})\ntarget_link_libraries(core PRIVATE\n{libs}  PUBLIC\n    z)\n# one\n");
        let v = view("CMakeLists.txt", &src, Kind::CMake).expect("view");
        assert_eq!(v, "CMakeLists.txt 32 lines; terse: 1 comments, 1 includes, 2 logging, 2 lists\n1-3\t⋯ comment, 3 line(s)\n4\tcmake_minimum_required(VERSION 3.20)\n5-7\t⋯ 3 include(s)\n11\tmessage(WARNING \"careful\")\n12\tadd_library(core\n13\t  src/f1.cpp\n14-19\t⋯ 6 item(s)\n20\t  src/f8.cpp\n21\t)\n22\ttarget_link_libraries(core PRIVATE\n23\t    lib1\n24-28\t⋯ 5 item(s)\n29\t    lib7\n30\t  PUBLIC\n31\t    z)\n32\t# one");
        assert!(view("a.cmake", "set(X 1)\n#[==[ open\n", Kind::CMake).is_none());
        assert!(view("a.cmake", "#[[ one ]]\nset(X 1)\n", Kind::CMake).is_none());
        let params: String = (1..=7).map(|n| format!("  p{n}\n")).collect();
        assert!(view("a.cmake", &format!("function(f\n{params})\n"), Kind::CMake).is_none());
        assert!(view("a.cmake", &format!("MACRO (f\n{params})\n"), Kind::CMake).is_none());
        assert!(view("a.cmake", &format!("execute_process(COMMAND\n{params})\n"), Kind::CMake).is_none());
        assert_eq!(kind_of(Path::new("cmakelists.TXT")), Some(Kind::CMake));
        assert_eq!(kind_of(Path::new("Toolchain.cmake")), Some(Kind::CMake));
        assert_eq!(kind_of(Path::new("a.txt")), None);
    }

    // Checks a Makefile: comments, includes, a continued source list, $(info) logging, and recipes and short lists that stay.
    #[test]
    fn test_view_make() {
        let srcs: String = (1..=7).map(|n| format!("\tsrc/f{n}.c \\\n")).collect();
        let src = format!("# Build\n# all\n# targets\ninclude a.mk\n-include b.mk\nsinclude c.mk\nSRCS = \\\n{srcs}\tsrc/main.c\n$(info building)\nall: $(SRCS)\n\t@echo done\nSHORT = a \\\n\tb\n");
        let v = view("Makefile", &src, Kind::Make).expect("view");
        assert_eq!(v, "Makefile 20 lines; terse: 1 comments, 1 includes, 1 lists, 1 logging\n1-3\t⋯ comment, 3 line(s)\n4-6\t⋯ 3 include(s)\n7\tSRCS = \\\n8\t\tsrc/f1.c \\\n9-14\t⋯ 6 item(s)\n15\t\tsrc/main.c\n17\tall: $(SRCS)\n18\t\t@echo done\n19\tSHORT = a \\\n20\t\tb");
        assert!(view("a.mk", "x = 1 \\\n", Kind::Make).is_none());
        for (name, kind) in
            [("GNUmakefile", Kind::Make), ("Makefile.am", Kind::Make), ("rules.mak", Kind::Make)]
        {
            assert_eq!(kind_of(Path::new(name)), Some(kind));
        }
    }

    // Checks the edges: an unclosed message, a mode that is not logging, depth inside strings and comments, and words.
    #[test]
    fn test_buildfile_edges() {
        assert_eq!(log_end(&["message(DEBUG \"a\""], 0, Kind::CMake), None);
        assert_eq!(log_end(&["message(\"STATUS\")"], 0, Kind::CMake), None);
        assert_eq!(log_end(&["message(", "  STATUS \"a\")", "x"], 0, Kind::CMake), Some(1));
        assert_eq!(log_end(&["message(", "  FATAL_ERROR \"a\")"], 0, Kind::CMake), None);
        assert_eq!(log_end(&["message("], 0, Kind::CMake), None);
        assert_eq!(log_end(&["$(info a) x"], 0, Kind::Make), None);
        assert_eq!(depth("f(\"(\\\"(\") # ("), 0);
        assert!(
            is_word("a.c \\") && !is_word("PRIVATE") && !is_word("a b") && !is_word("#x") && is_word("X")
        );
        assert!(!is_include("include(a", Kind::CMake) && !is_include("  include a", Kind::Make));
    }
}
