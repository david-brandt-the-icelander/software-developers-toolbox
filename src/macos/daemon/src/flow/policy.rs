// The repository's workflow policy: `.zazzle/flow.json` when the repository opted in, else what zazzle discovers (the build and test commands of the projects it finds, the default branch); the review level of each branch; and statements in the repository's guidance about which branch pull requests target, so a conflict between them is shown before anything is delivered.
use super::repo;
use serde_json::Value;
use std::path::{Path, PathBuf};

// The opt-in file, relative to the repository's top.
pub const FILE: &str = ".zazzle/flow.json";
// The review levels, least strict first.
pub const LEVELS: &[&str] = &["feature", "development", "staging", "production"];
// Branch names that read as targets in guidance text.
const BRANCHES: &[&str] =
    &["main", "master", "dev", "develop", "development", "staging", "stage", "production", "prod", "release"];

// One check the repository runs: its name, the shell command, the folder it runs in (relative to the top), its kind (build, test, coverage, format, lint or script), the paths whose change makes it apply (none: always), the command that fixes what it finds (formatters, linters' fix modes), and for coverage the report file it writes (lcov, Cobertura XML, a Go cover profile or coverage.py JSON), relative to its folder.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Check {
    pub name: String,
    pub run: String,
    pub dir: String,
    pub kind: String,
    pub paths: Vec<String>,
    pub fix: Option<String>,
    pub report: Option<String>,
}

// The check kinds.
pub const KINDS: &[&str] = &["build", "test", "coverage", "format", "lint", "audit", "script"];

// The resolved policy: whether the repository opted in, its checks, the branch pull requests target and where that came from, the level of each named branch, protected branches, the changelog file and its check script, the pull request template, the documentation paths and the commands that check documentation, the lockfiles' regeneration commands it adds to the usual ones, the effects deliver may have, the configured agents, and the conflicts found in guidance.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Policy {
    pub opted_in: bool,
    pub checks: Vec<Check>,
    pub target: String,
    pub target_from: String,
    pub levels: Vec<(String, String)>,
    pub protected: Vec<String>,
    pub changelog: Option<String>,
    pub changelog_script: Option<String>,
    pub pr_template: Option<String>,
    pub docs: Vec<String>,
    pub docs_checks: Vec<String>,
    pub lockfiles: Vec<(String, String)>,
    pub effects: Vec<String>,
    pub agents: Value,
    pub conflicts: Vec<String>,
    pub statements: Vec<(String, String)>,
}

impl Policy {
    // The review level of a branch: the configured one, else by its name (main, master, production, prod: production; staging, stage: staging; dev, develop, development: development; anything else: feature).
    pub fn level(&self, branch: &str) -> String {
        if let Some((_, l)) = self.levels.iter().find(|(b, _)| b == branch) {
            return l.clone();
        }
        match branch {
            "main" | "master" | "production" | "prod" => "production",
            "staging" | "stage" => "staging",
            "dev" | "develop" | "development" => "development",
            _ => "feature",
        }
        .into()
    }

    // Tells whether a branch must not be committed to directly: protected, or one with a level above feature.
    pub fn is_protected(&self, branch: &str) -> bool {
        self.protected.iter().any(|p| p == branch) || self.level(branch) != "feature"
    }

    // An agent spec configured under `agents.KEY`, if any.
    pub fn agent(&self, key: &str) -> Option<String> {
        self.agents[key].as_str().map(str::to_string)
    }

    // Agent specs configured under `agents.KEY` as a list (or one string).
    pub fn agent_list(&self, key: &str) -> Vec<String> {
        match &self.agents[key] {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        }
    }

    // Tells whether a path is documentation the writer keeps: under a configured docs path, or Markdown, reStructuredText or text, or a `.dev` record.
    pub fn is_doc(&self, path: &str) -> bool {
        let low = path.to_lowercase();
        (!self.docs.is_empty() && repo::under(path, &self.docs))
            || [".md", ".mdx", ".rst", ".txt"].iter().any(|e| low.ends_with(e))
            || low.starts_with(".dev/")
            || low.contains("/.dev/")
    }

    // The command that regenerates a lockfile from its manifest, by file name: the policy's own, else the usual one for Cargo, npm, Yarn, pnpm, Go, Poetry, Composer or Bundler.
    pub fn regenerate(&self, path: &str) -> Option<String> {
        let name = path.rsplit('/').next().unwrap_or(path);
        if let Some((_, c)) = self.lockfiles.iter().find(|(f, _)| f == name || f == path) {
            return Some(c.clone());
        }
        let usual = [
            ("Cargo.lock", "cargo metadata --format-version 1 > /dev/null"),
            ("package-lock.json", "npm install --package-lock-only --ignore-scripts"),
            ("yarn.lock", "yarn install --mode update-lockfile"),
            ("pnpm-lock.yaml", "pnpm install --lockfile-only"),
            ("go.sum", "go mod tidy"),
            ("poetry.lock", "poetry lock --no-update"),
            ("composer.lock", "composer update --lock"),
            ("Gemfile.lock", "bundle lock"),
        ];
        usual.iter().find(|(f, _)| *f == name).map(|(_, c)| c.to_string())
    }

    // The checks that apply to a set of changed paths.
    pub fn applicable(&self, changed: &[String]) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|c| c.paths.is_empty() || changed.iter().any(|p| repo::under(p, &c.paths)))
            .collect()
    }
}

// Reads the policy for the repository at `top`: the opt-in file when present (an unreadable one is an error), else discovery; then the guidance statements about targets and their conflicts.
pub fn load(top: &Path) -> Result<Policy, String> {
    let mut p = match std::fs::read_to_string(top.join(FILE)) {
        Ok(text) => {
            let v: Value = serde_json::from_str(&text).map_err(|e| format!("{FILE}: {e}"))?;
            configured(top, &v)?
        }
        Err(_) => Policy { checks: discover(top), effects: Vec::new(), ..Policy::default() },
    };
    if p.target.is_empty() {
        let def = default_branch(top);
        p.target_from = format!("the repository's default branch ({def})");
        p.target = def;
    }
    p.pr_template = p.pr_template.or_else(|| {
        [
            ".github/pull_request_template.md",
            ".github/PULL_REQUEST_TEMPLATE.md",
            "docs/pull_request_template.md",
        ]
        .iter()
        .find(|f| top.join(f).is_file())
        .map(|f| f.to_string())
    });
    p.changelog =
        p.changelog.or_else(|| top.join("CHANGELOG.md").is_file().then(|| "CHANGELOG.md".to_string()));
    p.statements = statements(top);
    let named: Vec<&String> = p.statements.iter().map(|(_, b)| b).fold(Vec::new(), |mut v, b| {
        if !v.contains(&b) {
            v.push(b)
        }
        v
    });
    let said = || p.statements.iter().map(|(at, b)| format!("{at} says {b}")).collect::<Vec<_>>().join("; ");
    if named.len() > 1 {
        p.conflicts.push(match p.opted_in && !p.target_from.starts_with("the repository's default") {
            true => format!(
                "the guidance disagrees on the pull request target ({}); {FILE} resolves it: {}",
                said(),
                p.target
            ),
            false => format!(
                "the guidance disagrees on the pull request target ({}); set branches.target in {FILE}",
                said()
            ),
        });
    } else if let Some(b) = named.first().filter(|b| ***b != p.target) {
        if p.opted_in {
            p.conflicts.push(format!(
                "the guidance says pull requests target {b} ({}), but {FILE} says {}",
                said(),
                p.target
            ));
        } else {
            p.target_from = format!("the guidance ({})", said());
            p.target = b.to_string();
        }
    }
    Ok(p)
}

// Reads an opt-in file.
fn configured(top: &Path, v: &Value) -> Result<Policy, String> {
    let strs = |x: &Value| {
        x.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect::<Vec<_>>()
    };
    let mut checks = Vec::new();
    for (i, c) in v["checks"].as_array().into_iter().flatten().enumerate() {
        let run = c["run"].as_str().ok_or_else(|| format!("{FILE}: checks[{i}] has no `run`"))?;
        let kind = c["kind"].as_str().unwrap_or("test");
        if !KINDS.contains(&kind) {
            return Err(format!("{FILE}: checks[{i}].kind is one of {}, not `{kind}`", KINDS.join(", ")));
        }
        checks.push(Check {
            name: c["name"].as_str().map_or_else(|| format!("check{}", i + 1), str::to_string),
            run: run.to_string(),
            dir: c["dir"].as_str().unwrap_or(".").to_string(),
            kind: kind.to_string(),
            paths: strs(&c["paths"]),
            fix: c["fix"].as_str().map(str::to_string),
            report: c["report"].as_str().map(str::to_string),
        });
    }
    if checks.is_empty() {
        checks = discover(top);
    }
    let b = &v["branches"];
    let mut levels = Vec::new();
    for (name, l) in b["levels"].as_object().into_iter().flatten() {
        let l = l.as_str().unwrap_or_default();
        if !LEVELS.contains(&l) {
            return Err(format!("{FILE}: branches.levels.{name} is one of {}, not `{l}`", LEVELS.join(", ")));
        }
        levels.push((name.clone(), l.to_string()));
    }
    let target = b["target"].as_str().unwrap_or_default().to_string();
    let effects = match v.get("effects") {
        Some(e) => strs(e),
        None => vec!["commit".into(), "push".into(), "pr".into()],
    };
    if let Some(bad) = effects.iter().find(|e| !["commit", "push", "pr"].contains(&e.as_str())) {
        return Err(format!("{FILE}: effects are commit, push and pr, not `{bad}`"));
    }
    Ok(Policy {
        opted_in: true,
        checks,
        target_from: if target.is_empty() { String::new() } else { FILE.to_string() },
        target,
        levels,
        protected: strs(&b["protected"]),
        changelog: v["changelog"]["path"].as_str().map(str::to_string),
        changelog_script: v["changelog"]["script"].as_str().map(str::to_string),
        pr_template: v["pr_template"].as_str().map(str::to_string),
        docs: strs(&v["docs"]["paths"]),
        docs_checks: strs(&v["docs"]["check"]),
        lockfiles: v["lockfiles"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, c)| Some((k.clone(), c.as_str()?.to_string())))
            .collect(),
        effects,
        agents: v["agents"].clone(),
        ..Policy::default()
    })
}

// The default branch: origin's HEAD when known, else main or master if they exist, else main.
fn default_branch(top: &Path) -> String {
    if let Ok(r) = repo::git(top, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        return r.trim_start_matches("origin/").to_string();
    }
    for b in ["main", "master"] {
        if repo::git(top, &["rev-parse", "--verify", "--quiet", b]).is_ok() {
            return b.into();
        }
    }
    "main".into()
}

// Finds the projects' checks without running anything (dependency audits apply when a manifest or lockfile changes): tests for Cargo workspaces, npm packages with a test script, Go modules, Python projects, .NET solutions and Makefiles with a test target, and the formatters and linters with their fix commands for Cargo (rustfmt, clippy), Go (gofmt, vet), .NET (dotnet format) and npm (a lint script), up to three folders deep, a folder inside a found Rust workspace skipped.
pub fn discover(top: &Path) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();
    let mut dirs = vec![(top.to_path_buf(), 0)];
    while let Some((d, depth)) = dirs.pop() {
        let rel = d
            .strip_prefix(top)
            .ok()
            .map(|r| r.to_string_lossy().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| ".".into());
        let inside = out
            .iter()
            .any(|c| c.run.starts_with("cargo") && c.dir != "." && rel.starts_with(&format!("{}/", c.dir)))
            || out.iter().any(|c| c.run.starts_with("cargo") && c.dir == "." && rel != ".");
        let has = |f: &str| d.join(f).is_file();
        let mut add = |name: &str, run: String, kind: &str, fix: Option<&str>, files: &[&str]| {
            let under = |f: &str| if rel == "." { f.to_string() } else { format!("{rel}/{f}") };
            let paths = match (files.is_empty(), rel == ".") {
                (true, true) => Vec::new(),
                (true, false) => vec![format!("{rel}/")],
                (false, _) => files.iter().map(|f| under(f)).collect(),
            };
            out.push(Check {
                name: if rel == "." { name.into() } else { format!("{name} {rel}") },
                run,
                dir: rel.clone(),
                kind: kind.into(),
                paths,
                fix: fix.map(str::to_string),
                report: None,
            });
        };
        if has("Cargo.toml") && !inside {
            add("cargo", "cargo test --workspace".into(), "test", None, &[]);
            add("rustfmt", "cargo fmt --all --check".into(), "format", Some("cargo fmt --all"), &[]);
            add("cargo audit", "cargo-audit audit".into(), "audit", None, &["Cargo.toml", "Cargo.lock"]);
            add(
                "clippy",
                "cargo clippy --workspace --all-targets -- -D warnings".into(),
                "lint",
                Some("cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged"),
                &[],
            );
        }
        let scripts = std::fs::read_to_string(d.join("package.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| v["scripts"].clone())
            .unwrap_or_default();
        if scripts["test"].is_string() {
            add("npm", "npm test".into(), "test", None, &[]);
        }
        if scripts["lint"].is_string() {
            add(
                "npm lint",
                "npm run lint".into(),
                "lint",
                scripts["lint:fix"].is_string().then_some("npm run lint:fix"),
                &[],
            );
        }
        if has("package-lock.json") {
            add(
                "npm audit",
                "npm audit --audit-level=high".into(),
                "audit",
                Some("npm audit fix"),
                &["package.json", "package-lock.json"],
            );
        }
        if has("go.mod") {
            add("go", "go test ./...".into(), "test", None, &[]);
            add("gofmt", "test -z \"$(gofmt -l .)\"".into(), "format", Some("gofmt -w ."), &[]);
            add("go vet", "go vet ./...".into(), "lint", None, &[]);
            add("govulncheck", "govulncheck ./...".into(), "audit", None, &["go.mod", "go.sum"]);
        }
        if has("pyproject.toml") || has("pytest.ini") || has("setup.cfg") {
            add("pytest", "python3 -m pytest -q".into(), "test", None, &[]);
            add(
                "pip-audit",
                "pip-audit".into(),
                "audit",
                None,
                &["pyproject.toml", "requirements.txt", "poetry.lock"],
            );
        }
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&d).into_iter().flatten().flatten().map(|e| e.path()).collect();
        entries.sort();
        if let Some(sln) = entries.iter().find(|p| p.extension().is_some_and(|e| e == "sln")) {
            let sln = sln.file_name().unwrap_or_default().to_string_lossy().to_string();
            add("dotnet", format!("dotnet test {sln}"), "test", None, &[]);
            add(
                "dotnet format",
                format!("dotnet format {sln} --verify-no-changes"),
                "format",
                Some(&format!("dotnet format {sln}")),
                &[],
            );
            add("dotnet audit", format!("! dotnet list {sln} package --vulnerable --include-transitive | grep -q 'has the following vulnerable packages'"), "audit", None, &[]);
        }
        if has("Makefile")
            && std::fs::read_to_string(d.join("Makefile"))
                .is_ok_and(|t| t.lines().any(|l| l.starts_with("test:")))
        {
            add("make", "make test".into(), "test", None, &[]);
        }
        if depth < 3 {
            for e in entries.iter().rev().filter(|p| p.is_dir()) {
                let n = e.file_name().unwrap_or_default().to_string_lossy();
                if !n.starts_with('.')
                    && !["node_modules", "target", "bin", "obj", "vendor", "dist", "build"]
                        .contains(&n.as_ref())
                {
                    dirs.push((e.clone(), depth + 1));
                }
            }
        }
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir).then(a.name.cmp(&b.name)));
    out
}

// The lines of the repository's guidance (AGENTS.md, CLAUDE.md, skills and agent docs, .github markdown) that say which branch pull requests target, as (`file:line`, branch).
pub fn statements(top: &Path) -> Vec<(String, String)> {
    let mut files = vec!["AGENTS.md".to_string(), "CLAUDE.md".to_string(), "CONTRIBUTING.md".to_string()];
    for d in ["skills", "agents", ".github", ".claude/skills", "docs"] {
        collect_md(&top.join(d), top, &mut files, 0);
    }
    let mut out = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(top.join(&f)) else { continue };
        for (i, line) in text.lines().enumerate() {
            let l = line.to_lowercase();
            let about_pr = l.contains("pull request") || l.contains(" pr") || l.contains("prs");
            let targets = ["target", "into", "against", "base", "open"].iter().any(|w| l.contains(w));
            if !(about_pr && targets) {
                continue;
            }
            let words: Vec<String> = l
                .split(|c: char| !matches!(c, 'a'..='z' | '0'..='9' | '-' | '_'))
                .map(str::to_string)
                .collect();
            for w in &words {
                if BRANCHES.contains(&w.as_str())
                    && ["`", "'"].iter().any(|q| l.contains(&format!("{q}{w}{q}")))
                {
                    out.push((format!("{f}:{}", i + 1), w.clone()));
                    break;
                }
            }
        }
    }
    out
}

// Adds the markdown files under a folder (three levels deep) as paths relative to the top.
fn collect_md(dir: &Path, top: &Path, out: &mut Vec<String>, depth: usize) {
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() && depth < 3 {
            collect_md(&p, top, out, depth + 1);
        } else if p.extension().is_some_and(|e| e == "md") {
            out.push(p.strip_prefix(top).unwrap_or(&p).to_string_lossy().to_string());
        }
    }
}
