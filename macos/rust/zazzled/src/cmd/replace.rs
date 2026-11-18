// The replace command: a regular-expression replacement across files, previewed with --dry-run, each file written only if it did not change since it was read.
use super::edit::{commit, open_target};
use super::find::build_regex;
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::meta::line_starts;
use crate::util::{display, locate};
use crate::walk::{self, WalkOpts};
use regex::bytes::NoExpand;

const SPEC: &[Flag] = &[
    opt("glob", 'g'),
    flag("dry-run", 'n'),
    flag("ignore-case", 'i'),
    flag("fixed", 'F'),
    flag("no-ignore", 'u'),
];
const USAGE: &str = "usage: replace PATTERN REPLACEMENT [PATH..] [-g GLOB].. [--dry-run] [-i] [-F] [-u]";
const PREVIEW: usize = 20;

// Runs the replace command.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    let [pattern, replacement, paths @ ..] = p.positional.as_slice() else { return Err(USAGE.into()) };
    let re = build_regex(pattern, p.has("ignore-case"), p.has("fixed"), false)?;
    let roots: Vec<_> = if paths.is_empty() {
        vec![ctx.cwd.clone()]
    } else {
        paths.iter().map(|a| locate(a, &ctx.cwd)).collect()
    };
    let opts = WalkOpts {
        no_ignore: p.has("no-ignore"),
        globs: p.values("glob").iter().map(|s| s.to_string()).collect(),
    };
    let files = walk::collect(&roots, &opts, &ctx.engine.walks)?;
    let dry = p.has("dry-run");
    let _guard = ctx.engine.edit_guard();
    let (mut lines, mut total, mut changed) = (Vec::new(), 0, 0);
    for f in &files {
        let Ok(data) = std::fs::read(f) else { continue };
        if crate::meta::looks_binary(&data) || !re.is_match(&data) {
            continue;
        }
        let starts = line_starts(&data);
        let at: Vec<usize> =
            re.find_iter(&data).map(|m| starts.partition_point(|s| (*s as usize) <= m.start())).collect();
        let new = if p.has("fixed") {
            re.replace_all(&data, NoExpand(replacement.as_bytes()))
        } else {
            re.replace_all(&data, replacement.as_bytes())
        };
        let shown = display(f, &ctx.cwd);
        if !dry {
            let t = open_target(ctx, &shown)?;
            if t.data != data {
                return Err(format!(
                    "{shown}: changed while replacing; run it again ({changed} files written before it)"
                ));
            }
            commit(ctx, &t, new.into_owned())?;
        } else {
            let new_starts = line_starts(&new);
            for line in at.iter().take(PREVIEW.saturating_sub(lines.len())) {
                let text = crate::meta::line_bytes(&new, &new_starts, *line);
                lines.push(format!("{shown}:{line}: {}", String::from_utf8_lossy(text).trim_end()));
            }
        }
        if !dry {
            lines.push(format!(
                "{shown}: {} at line(s) {}",
                at.len(),
                at.iter().map(usize::to_string).collect::<Vec<_>>().join(", ")
            ));
        }
        total += at.len();
        changed += 1;
    }
    if total == 0 {
        return Ok(Output::plain(format!("no matches for {pattern} in {} files", files.len())));
    }
    let head = match dry {
        true => format!("dry run: {total} replacements in {changed} files would be made; nothing written"),
        false => format!("replaced {total} in {changed} files"),
    };
    Ok(Output::plain(format!("{head}\n{}", lines.join("\n"))))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::testutil::{run_cmd, TempDir};

    // Checks a dry run writes nothing, a replacement uses capture groups across files and globs, and -F takes the text as it is.
    #[test]
    fn test_replace() {
        let t = TempDir::new();
        t.write("a.rs", b"fn old_name() {}\nold_name();\n");
        t.write("b.rs", b"old_name();\n");
        t.write("c.txt", b"old_name\n");
        let dry = run_cmd(t.path(), &["replace", "old_(\\w+)", "new_$1", "-g", "*.rs", "--dry-run"]).text;
        assert_eq!(dry, "dry run: 3 replacements in 2 files would be made; nothing written\na.rs:1: fn new_name() {}\na.rs:2: new_name();\nb.rs:1: new_name();");
        assert_eq!(
            std::fs::read_to_string(t.path().join("a.rs")).expect("a"),
            "fn old_name() {}\nold_name();\n"
        );
        let done = run_cmd(t.path(), &["replace", "old_(\\w+)", "new_$1", "-g", "*.rs"]).text;
        assert_eq!(done, "replaced 3 in 2 files\na.rs: 2 at line(s) 1, 2\nb.rs: 1 at line(s) 1");
        assert_eq!(std::fs::read_to_string(t.path().join("c.txt")).expect("c"), "old_name\n");
        assert_eq!(
            run_cmd(t.path(), &["replace", "old_name", "$x", "c.txt", "-F"]).text,
            "replaced 1 in 1 files\nc.txt: 1 at line(s) 1"
        );
        assert_eq!(std::fs::read_to_string(t.path().join("c.txt")).expect("c"), "$x\n");
        assert_eq!(run_cmd(t.path(), &["replace", "zzz", "y"]).text, "no matches for zzz in 3 files");
        assert!(run_cmd(t.path(), &["replace", "x"]).text.contains("usage: replace"));
        assert!(run_cmd(t.path(), &["replace", "(", "y"]).text.starts_with("error: "));
    }
}
