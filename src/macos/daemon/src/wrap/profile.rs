// Command profiles for zazzle run: known noise each tool prints (passing tests, progress, framework frames), dropped and counted.
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

// What a rule does with the lines it matches.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Drop,
    DropRepeat,
    Group,
}

// One rule: the lines it matches, what it does with them, and how dropped lines are counted or, for Group, the template a line becomes under its `path` heading.
struct Rule {
    action: Action,
    pattern: &'static str,
    label: &'static str,
}

// A named set of rules for one tool.
pub struct Profile {
    pub name: &'static str,
    rules: &'static [Rule],
}

// Lines a profile dropped, counted per label in first-seen order.
#[derive(Debug, Default, PartialEq)]
pub struct Dropped(pub Vec<(&'static str, usize)>);

// Shorthand for a rule that drops every matching line.
const fn drop(pattern: &'static str, label: &'static str) -> Rule {
    Rule { action: Action::Drop, pattern, label }
}

const PASSING: &str = "passing tests";
const PROGRESS: &str = "progress lines";
const FRAMES: &str = "framework stack frames";

const CARGO: &[Rule] = &[
    drop(
        r"^\s+(Compiling|Checking|Downloaded|Downloading|Documenting|Fresh|Locking|Updating|Adding|Removing|Packaging|Verifying|Unpacking|Fetching|Blocking waiting|Running (unittests )?\S+ \(|Doc-tests )",
        PROGRESS,
    ),
    drop(r"^running \d+ tests?$", PROGRESS),
    drop(r"^test .+ \.\.\. ok$", PASSING),
    drop(r"^\s+PASS \[", PASSING),
    drop(r"^test result: ok\. 0 passed; 0 failed; 0 ignored;", "empty test runs"),
    drop(r"^\s+\d+: (__rustc|core|std|alloc|test)::", FRAMES),
    drop(r"^\s+\d+: <.* as core::ops::function::Fn", FRAMES),
    drop(r"^\s+at /rustc/", FRAMES),
];
const PYTEST: &[Rule] = &[
    drop(r"^(platform |cachedir: |rootdir: |configfile: |plugins: )", "header lines"),
    drop(r"^\S+::\S+ PASSED\b", PASSING),
    drop(r"^\S+ [.sx]+\s*\[\s*\d+%\]$", PROGRESS),
];
const GO: &[Rule] = &[
    drop(r"^ok\s+\S+\s+(\(cached\)|[\d.]+s)", "passing packages"),
    drop(r"^\s*--- PASS: ", PASSING),
    drop(r"^=== (RUN|PAUSE|CONT|NAME) ", PROGRESS),
    drop(r"^PASS$", PROGRESS),
    drop(r"^\?\s+\S+\s+\[no test files\]$", "packages without tests"),
];
const DOTNET: &[Rule] = &[
    drop(
        r"^\s+(Determining projects to restore|All projects are up-to-date for restore|Restored )",
        PROGRESS,
    ),
    drop(r"^\s+\S+ -> \S+\.(dll|exe)$", PROGRESS),
    drop(
        r"^(MSBuild version |Test run for |Microsoft \(R\) Test Execution|Copyright \(c\) Microsoft|Starting test execution|A total of \d+ test files? matched)",
        "banner lines",
    ),
    drop(r"^\s+Passed \S", PASSING),
    drop(r"^\s+at (System|Microsoft|Xunit|NUnit)\.", FRAMES),
    Rule {
        action: Action::DropRepeat,
        pattern: r": (warning|error) [A-Za-z]+\d+:",
        label: "repeated diagnostics",
    },
];
const JS_TEST: &[Rule] = &[
    drop(r"^\s*PASS\s+\S", "passing files"),
    drop(r"^\s*[✓√✔] ", PASSING),
    drop(r"^\s*RUN\s+v\d", "banner lines"),
];
const INSTALL: &[Rule] = &[
    drop(
        r"^\s*(Collecting|Downloading|Using cached|Requirement already satisfied|Obtaining|Preparing metadata|Building wheel|Created wheel|Stored in directory|Installing build dependencies|Getting requirements|Looking in indexes)\b",
        PROGRESS,
    ),
    drop(r"^npm (http fetch|sill|timing|verb) ", PROGRESS),
];

const TSC: &[Rule] = &[
    drop(
        r"^\d{1,2}:\d{2}:\d{2}( [AP]M)? - (Projects in this build|Project '.+' is (out of date|up to date|being forcibly rebuilt)|Building project|Updating (unchanged )?output|Skipping build)",
        PROGRESS,
    ),
    drop(r"^\s+\* \S+\.json$", PROGRESS),
    drop(r"^Errors\s+Files$", "summary rows"),
    drop(r"^\s+\d+\s+\S+:\d+$", "summary rows"),
    Rule {
        action: Action::Group,
        pattern: r"^(?P<path>[^\s(]+\.(?:[cm]?[jt]sx?|vue))\((?P<line>\d+),(?P<col>\d+)\): (?P<msg>.+)$",
        label: "${line}:${col} ${msg}",
    },
    Rule {
        action: Action::Group,
        pattern: r"^(?P<path>\S+\.(?:[cm]?[jt]sx?|vue)):(?P<line>\d+):(?P<col>\d+) - (?P<msg>.+)$",
        label: "${line}:${col} ${msg}",
    },
];

pub const PROFILES: &[Profile] = &[
    Profile { name: "cargo", rules: CARGO },
    Profile { name: "pytest", rules: PYTEST },
    Profile { name: "go", rules: GO },
    Profile { name: "dotnet", rules: DOTNET },
    Profile { name: "js-test", rules: JS_TEST },
    Profile { name: "install", rules: INSTALL },
    Profile { name: "tsc", rules: TSC },
];

// Returns a profile by name.
pub fn named(name: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.name == name)
}

// Returns a program's file name without directories or a Windows .exe suffix.
fn base(prog: &str) -> &str {
    let b = prog.rsplit('/').next().unwrap_or(prog);
    b.strip_suffix(".exe").unwrap_or(b)
}

// Skips launchers that run another program (npx, uv run, python -m, env), returning the real command line.
fn unwrap_launcher(argv: &[String]) -> &[String] {
    let Some(first) = argv.first() else { return argv };
    let rest = &argv[1..];
    let sub = rest.first().map(String::as_str);
    match (base(first), sub) {
        ("npx" | "bunx", _) => unwrap_launcher(rest),
        ("env", _) => {
            let skip = rest.iter().take_while(|a| a.contains('=') || a.starts_with('-')).count();
            unwrap_launcher(&rest[skip..])
        }
        ("uv" | "poetry" | "pdm" | "pipenv", Some("run")) | ("pnpm" | "yarn", Some("exec" | "dlx")) => {
            unwrap_launcher(&rest[1..])
        }
        (p, Some("-m")) if p.starts_with("python") => unwrap_launcher(&rest[1..]),
        _ => argv,
    }
}

// Picks the profile for a command line from its program and subcommand.
pub fn detect(argv: &[String]) -> Option<&'static Profile> {
    let argv = unwrap_launcher(argv);
    let prog = base(argv.first()?);
    let sub = argv.get(1).map(String::as_str);
    let name = match (prog, sub) {
        ("cargo" | "cargo-nextest", _) => "cargo",
        ("pytest" | "py.test", _) => "pytest",
        ("go", Some("test" | "vet" | "build")) => "go",
        ("dotnet", _) => "dotnet",
        ("tsc" | "vue-tsc", _) => "tsc",
        ("jest" | "vitest" | "mocha", _) => "js-test",
        ("npm" | "pnpm" | "yarn" | "bun", Some("test" | "t")) => "js-test",
        ("npm" | "pnpm" | "yarn" | "bun", Some("install" | "i" | "ci" | "add")) => "install",
        ("pip" | "pip3", Some("install")) => "install",
        ("uv", Some("pip")) => "install",
        _ => return None,
    };
    named(name)
}

// Returns the compiled patterns of every rule, built once.
fn compiled() -> &'static Vec<Vec<Regex>> {
    static RE: OnceLock<Vec<Vec<Regex>>> = OnceLock::new();
    RE.get_or_init(|| {
        PROFILES
            .iter()
            .map(|p| p.rules.iter().map(|r| Regex::new(r.pattern).expect("valid rule pattern")).collect())
            .collect()
    })
}

impl Dropped {
    // Adds one dropped line under a label.
    fn count(&mut self, label: &'static str) {
        match self.0.iter_mut().find(|(l, _)| *l == label) {
            Some((_, n)) => *n += 1,
            None => self.0.push((label, 1)),
        }
    }

    // Describes the counts, such as `412 passing tests, 87 progress lines`.
    pub fn describe(&self) -> String {
        self.0.iter().map(|(l, n)| format!("{n} {l}")).collect::<Vec<_>>().join(", ")
    }
}

impl Profile {
    // Drops the lines this profile's rules match and counts them, groups diagnostics under their file, and keeps every other line as it is.
    pub fn apply(&self, lines: Vec<String>) -> (Vec<String>, Dropped) {
        let at = PROFILES.iter().position(|p| p.name == self.name).expect("profile is listed");
        let patterns = &compiled()[at];
        let (mut kept, mut dropped, mut seen) =
            (Vec::with_capacity(lines.len()), Dropped::default(), HashSet::new());
        let mut heading = String::new();
        'lines: for line in lines {
            for (rule, re) in self.rules.iter().zip(patterns) {
                if rule.action == Action::Group {
                    let Some(c) = re.captures(&line) else { continue };
                    if heading != c["path"] {
                        heading = c["path"].to_string();
                        kept.push(heading.clone());
                    }
                    let mut grouped = String::new();
                    c.expand(rule.label, &mut grouped);
                    kept.push(grouped);
                    continue 'lines;
                }
                if !re.is_match(&line) {
                    continue;
                }
                if rule.action == Action::Drop || !seen.insert(line.clone()) {
                    dropped.count(rule.label);
                    continue 'lines;
                }
            }
            kept.push(line);
        }
        (kept, dropped)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Converts string slices into owned strings.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Applies a named profile to text and returns the kept text and the description of what was dropped.
    fn run(name: &str, text: &str) -> (String, String) {
        let (kept, dropped) = named(name).expect("profile").apply(text.lines().map(str::to_string).collect());
        (kept.join("\n"), dropped.describe())
    }

    // Checks the profile chosen for common command lines, through launchers, and none for others.
    #[test]
    fn test_detect() {
        let d = |a: &[&str]| detect(&v(a)).map(|p| p.name);
        assert_eq!(d(&["cargo", "test", "-p", "x"]), Some("cargo"));
        assert_eq!(d(&["/usr/bin/cargo", "clippy"]), Some("cargo"));
        assert_eq!(d(&["pytest", "-q"]), Some("pytest"));
        assert_eq!(d(&["python3", "-m", "pytest"]), Some("pytest"));
        assert_eq!(d(&["uv", "run", "pytest"]), Some("pytest"));
        assert_eq!(d(&["go", "test", "./..."]), Some("go"));
        assert_eq!(d(&["dotnet.exe", "test"]), Some("dotnet"));
        assert_eq!(d(&["npx", "vitest", "run"]), Some("js-test"));
        assert_eq!(d(&["pnpm", "exec", "jest"]), Some("js-test"));
        assert_eq!(d(&["npm", "test"]), Some("js-test"));
        assert_eq!(d(&["env", "CI=1", "npm", "ci"]), Some("install"));
        assert_eq!(d(&["pip3", "install", "x"]), Some("install"));
        assert_eq!(d(&["uv", "pip", "install", "x"]), Some("install"));
        assert_eq!(d(&["python", "-m", "pip", "install", "x"]), Some("install"));
        assert_eq!(d(&["npx", "tsc", "--noEmit"]), Some("tsc"));
        assert_eq!(d(&["vue-tsc", "-b"]), Some("tsc"));
        assert_eq!(d(&["go", "run", "."]), None);
        assert_eq!(d(&["npm", "run", "build"]), None);
        assert_eq!(d(&["make"]), None);
        assert_eq!(d(&[]), None);
        assert_eq!(d(&["npx"]), None);
    }

    // Checks cargo test output loses progress, passing tests, empty runs and std frames but keeps failures.
    #[test]
    fn test_cargo() {
        let out = "   Compiling demo v0.1.0 (/x)\n    Finished `test` profile [unoptimized] target(s) in 0.70s\n     Running unittests src/lib.rs (target/debug/deps/demo-1)\n\nrunning 3 tests\ntest t::ok1 ... ok\ntest t::skip ... ignored\ntest t::bad ... FAILED\n\n---- t::bad stdout ----\nthread 't::bad' panicked at src/lib.rs:6:18:\n   0: __rustc::rust_begin_unwind\n             at /rustc/abc/library/std/src/panicking.rs:689:5\n   1: core::panicking::panic_fmt\n   4: demo::t::bad\n             at ./src/lib.rs:6:18\n   6: <demo::t::bad::{closure#0} as core::ops::function::FnOnce<()>>::call_once\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out\n     Running tests/it.rs (target/debug/deps/it-2)\nrunning 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n        PASS [   0.004s] demo t::ok1\nerror: test failed, to rerun pass `--lib`";
        let (kept, dropped) = run("cargo", out);
        assert_eq!(
            kept,
            "    Finished `test` profile [unoptimized] target(s) in 0.70s\n\ntest t::skip ... ignored\ntest t::bad ... FAILED\n\n---- t::bad stdout ----\nthread 't::bad' panicked at src/lib.rs:6:18:\n   4: demo::t::bad\n             at ./src/lib.rs:6:18\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out\nerror: test failed, to rerun pass `--lib`"
        );
        assert_eq!(dropped, "5 progress lines, 2 passing tests, 4 framework stack frames, 1 empty test runs");
    }

    // Checks pytest drops its header, PASSED lines and all-pass progress lines, keeping a line with a failure.
    #[test]
    fn test_pytest() {
        let out = "platform linux -- Python 3.13\nrootdir: /x\ncollected 3 items\n\ntest_x.py .F.                     [100%]\ntest_y.py ...                     [100%]\ntest_x.py::test_a PASSED          [ 33%]\ntest_x.py::test_b FAILED          [ 66%]\nFAILED test_x.py::test_b - assert 1 == 2";
        let (kept, dropped) = run("pytest", out);
        assert_eq!(kept, "collected 3 items\n\ntest_x.py .F.                     [100%]\ntest_x.py::test_b FAILED          [ 66%]\nFAILED test_x.py::test_b - assert 1 == 2");
        assert_eq!(dropped, "2 header lines, 1 progress lines, 1 passing tests");
    }

    // Checks go test drops passing packages and tests, run markers and packages without tests.
    #[test]
    fn test_go() {
        let out = "=== RUN   TestA\n--- PASS: TestA (0.00s)\n=== RUN   TestB\n    a_test.go:4: boom\n--- FAIL: TestB (0.00s)\nFAIL\nFAIL\tdemo\t0.002s\n?   \tdemo/empty\t[no test files]\nPASS\nok  \tdemo/sub\t0.002s\nok  \tdemo/c\t(cached)";
        let (kept, dropped) = run("go", out);
        assert_eq!(kept, "    a_test.go:4: boom\n--- FAIL: TestB (0.00s)\nFAIL\nFAIL\tdemo\t0.002s");
        assert_eq!(
            dropped,
            "3 progress lines, 1 passing tests, 1 packages without tests, 2 passing packages"
        );
    }

    // Checks dotnet drops restore and banner lines, passing tests and framework frames, and repeated diagnostics.
    #[test]
    fn test_dotnet() {
        let w = "/x/A.cs(7,30): warning CS0168: The variable 'u' is declared but never used [/x/D.csproj]";
        let out = format!("MSBuild version 17.8\n  Determining projects to restore...\n{w}\n  Demo -> /x/bin/Demo.dll\nTest run for /x/Demo.dll\n  Passed Demo.T.Ok [1 ms]\n  Failed Demo.T.Bad [4 ms]\n  Stack Trace:\n     at Demo.T.Bad() in /x/T.cs:line 6\n   at System.RuntimeMethodHandle.InvokeMethod(Object target)\n\nBuild succeeded.\n{w}\n    1 Warning(s)");
        let (kept, dropped) = run("dotnet", &out);
        assert_eq!(kept, format!("{w}\n  Failed Demo.T.Bad [4 ms]\n  Stack Trace:\n     at Demo.T.Bad() in /x/T.cs:line 6\n\nBuild succeeded.\n    1 Warning(s)"));
        assert_eq!(dropped, "2 banner lines, 2 progress lines, 1 passing tests, 1 framework stack frames, 1 repeated diagnostics");
    }

    // Checks the JavaScript runners and installers drop their passing and progress lines.
    #[test]
    fn test_js_and_install() {
        let (kept, dropped) = run("js-test", " RUN  v5.1.0 /x\nPASS ./b.test.js\n ✓ v.test.mjs > a 2ms\n × v.test.mjs > b 6ms\nFAIL ./a.test.js");
        assert_eq!(kept, " × v.test.mjs > b 6ms\nFAIL ./a.test.js");
        assert_eq!(dropped, "1 banner lines, 1 passing files, 1 passing tests");
        let (kept, dropped) = run("install", "Collecting x\n  Downloading x-1.whl (1 MB)\nnpm sill fetch\nERROR: no matching distribution\nSuccessfully installed x-1");
        assert_eq!(kept, "ERROR: no matching distribution\nSuccessfully installed x-1");
        assert_eq!(dropped, "3 progress lines");
    }

    // Checks plain tsc diagnostics are grouped under their file and build-mode progress is dropped.
    #[test]
    fn test_tsc_plain() {
        let out = "1:27:38 PM - Projects in this build: \n    * tsconfig.json\n1:27:38 PM - Project 'tsconfig.json' is being forcibly rebuilt\n1:27:38 PM - Building project '/x/tsconfig.json'...\nsrc/a.ts(1,14): error TS2322: Type 'string' is not assignable to type 'number'.\nsrc/a.ts(3,41): error TS2339: Property 'foo' does not exist on type 'string'.\nsrc/b.vue(2,14): error TS2322: Type 'number' is not assignable to type 'string'.\nsrc/a.ts(9,1): error TS1005: ';' expected.";
        let (kept, dropped) = run("tsc", out);
        assert_eq!(
            kept,
            "src/a.ts\n1:14 error TS2322: Type 'string' is not assignable to type 'number'.\n3:41 error TS2339: Property 'foo' does not exist on type 'string'.\nsrc/b.vue\n2:14 error TS2322: Type 'number' is not assignable to type 'string'.\nsrc/a.ts\n9:1 error TS1005: ';' expected."
        );
        assert_eq!(dropped, "4 progress lines");
    }

    // Checks pretty tsc diagnostics keep their code frames under the file heading and the summary table goes.
    #[test]
    fn test_tsc_pretty() {
        let out = "src/a.ts:1:14 - error TS2322: Type 'string' is not assignable to type 'number'.\n\n1 export const a: number = \"x\";\n               ~\n\nsrc/a.ts:2:14 - error TS2322: Nope.\n\nFound 2 errors in 1 file.\n\nErrors  Files\n     2  src/a.ts:1";
        let (kept, dropped) = run("tsc", out);
        assert_eq!(
            kept,
            "src/a.ts\n1:14 error TS2322: Type 'string' is not assignable to type 'number'.\n\n1 export const a: number = \"x\";\n               ~\n\n2:14 error TS2322: Nope.\n\nFound 2 errors in 1 file.\n"
        );
        assert_eq!(dropped, "2 summary rows");
    }

    // Checks the description of nothing dropped is empty.
    #[test]
    fn test_describe_empty() {
        assert_eq!(Dropped::default().describe(), "");
    }
}
