// The outline operation: the daemon's symbols per file as [start, end, kind, name], with nested names dotted.
use crate::engine::Ctx;
use crate::host::{display, hidden, resolve, Symbol};
use crate::req::OutlineArgs;
use crate::state::{Res, State};
use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_SYMBOLS: usize = 300;

// Renders symbols as rows, naming a nested symbol after the symbols that enclose it (`A.go`).
fn rows(symbols: &[Symbol]) -> Vec<Value> {
    let mut scope: Vec<&str> = vec![];
    symbols
        .iter()
        .map(|s| {
            scope.truncate(s.depth);
            let name = match scope.is_empty() {
                true => s.name.clone(),
                false => format!("{}.{}", scope.join("."), s.name),
            };
            scope.push(&s.name);
            json!([s.start, s.end, s.kind, name])
        })
        .collect()
}

// Expands the paths into files: a file stands for itself, a directory for its visible files.
fn targets(ctx: &Ctx, a: &OutlineArgs) -> Res<Vec<PathBuf>> {
    let paths = if a.paths.is_empty() { vec![".".to_string()] } else { a.paths.clone() };
    let mut files = vec![];
    for p in paths {
        let root = resolve(ctx.cwd, &p);
        if root.is_file() {
            files.push(root);
            continue;
        }
        let roots = [root];
        let found = ctx.host.files(&roots, &[], false)?;
        files.extend(found.into_iter().filter(|f| !hidden(f, &roots)));
    }
    Ok(files)
}

// Outlines every file and builds the reply, keeping at most max_symbols rows.
fn compute(ctx: &Ctx, a: &OutlineArgs) -> Res<Value> {
    let files = targets(ctx, a)?;
    let cap = a.max_symbols.unwrap_or(MAX_SYMBOLS);
    let mut outlined: Vec<(String, Vec<Value>)> = files
        .iter()
        .zip(ctx.host.outline(&files))
        .filter_map(|(f, syms)| Some((display(f, ctx.cwd), rows(&syms?))))
        .filter(|(_, rows)| !rows.is_empty())
        .collect();
    outlined.sort_by(|x, y| x.0.cmp(&y.0));
    let total: usize = outlined.iter().map(|o| o.1.len()).sum();
    let (mut left, mut out) = (cap, vec![]);
    for (path, mut rows) in outlined {
        rows.truncate(left);
        left -= rows.len();
        if !rows.is_empty() {
            out.push(json!({"path": path, "symbols": rows}));
        }
    }
    let mut value = json!({"files": out, "total_symbols": total});
    if total > cap {
        value["truncated"] = json!(true);
    }
    Ok(value)
}

// Run an outline request, answering unchanged when this session already has the same outline.
pub fn run(st: &mut State, ctx: &Ctx, a: OutlineArgs) -> Res<Value> {
    let value = compute(ctx, &a)?;
    let fresh = a.fresh;
    let keyed = OutlineArgs { fresh: false, ..a };
    Ok(st.serve(&format!("outline|{}|{keyed:?}", ctx.cwd.display()), fresh, value))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::host::testing::Plain;
    use crate::state::{Sent, Shared};
    use std::path::Path;

    // Runs an outline request in a directory with a fresh session.
    fn outline(cwd: &Path, v: Value) -> Res<Value> {
        let (mut sh, mut sent) = (Shared::default(), Sent::default());
        let a: OutlineArgs = serde_json::from_value(v).unwrap();
        run(&mut State { shared: &mut sh, sent: &mut sent }, &Ctx { cwd, host: &Plain }, a)
    }

    // Nested symbols are dotted after their containers and the scope closes when depth falls.
    #[test]
    fn test_rows_dotted_names() {
        let sym = |depth: usize, kind: &str, name: &str| Symbol {
            start: 1,
            end: 1,
            depth,
            kind: kind.into(),
            name: name.into(),
        };
        let got = rows(&[sym(0, "impl", "A"), sym(1, "fn", "go"), sym(0, "fn", "top")]);
        let names: Vec<&str> = got.iter().map(|r| r[3].as_str().unwrap()).collect();
        assert_eq!(names, ["A", "A.go", "top"]);
    }

    // Directories outline their supported visible files, named files stand alone, and the cap truncates.
    #[test]
    fn test_outline_files_dirs_and_cap() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "impl A\n    fn go()\nfn top()\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "// nothing\n").unwrap();
        std::fs::write(dir.path().join("c.txt"), "fn not_rust()\n").unwrap();
        std::fs::create_dir(dir.path().join(".hid")).unwrap();
        std::fs::write(dir.path().join(".hid/d.rs"), "fn hidden()\n").unwrap();
        let all = outline(dir.path(), json!({})).unwrap();
        assert_eq!(
            all["files"],
            json!([{"path": "a.rs", "symbols": [[1, 1, "impl", "A"], [2, 2, "fn", "A.go"], [3, 3, "fn", "top"]]}])
        );
        let one = outline(dir.path(), json!({"paths": ["a.rs"], "max_symbols": 2})).unwrap();
        assert_eq!((one["truncated"].as_bool(), one["total_symbols"].as_u64()), (Some(true), Some(3)));
        let none = outline(dir.path(), json!({"paths": ["a.rs", "b.rs"], "max_symbols": 0})).unwrap();
        assert_eq!(none["files"], json!([]));
        assert!(outline(dir.path(), json!({"paths": ["missing"]})).is_err());
    }
}
