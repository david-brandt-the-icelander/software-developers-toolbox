// Makes the repository the tool just changed keep that layout: the skill, an AGENTS.md section and a pre-commit check.
using System.Text.RegularExpressions;

namespace ZazzleMinifier;

// Writes the skill, the AGENTS.md section and the pre-commit block for one root, merging switches from earlier runs.
internal static class RepoSetup
{
    // The commands in the order they run.
    private static readonly string[] AllSwitches = ["--strip-comments", "--split-tests", "--split-headers", "--restructure"];

    // Sets up the repository holding root and returns one line per file it created or changed.
    public static List<string> Run(string root, IReadOnlyList<string> switches)
    {
        var repo = RepoTop(root);
        var relRoot = Path.GetRelativePath(repo, root).Replace('\\', '/');
        var hookPath = HookPath(repo);
        var known = hookPath == null ? [] : KnownSwitches(hookPath, relRoot);
        var all = AllSwitches.Where(s => switches.Contains(s) || known.Contains(s)).ToList();
        var changed = new List<string>();
        var skillRel = SkillRel(repo);
        Ensure(repo, skillRel, SkillText(), changed);
        var restructured = RestructuredProjects(root).Select(d => Path.GetRelativePath(repo, d).Replace('\\', '/')).ToList();
        Ensure(repo, "AGENTS.md", Merge(ReadOrEmpty(Path.Combine(repo, "AGENTS.md")), relRoot, AgentsSection(relRoot, skillRel, all, restructured)), changed);
        if (!File.Exists(Path.Combine(repo, "CLAUDE.md")))
            Ensure(repo, "CLAUDE.md", "@AGENTS.md\n", changed);
        if (hookPath == null)
            return changed;
        var hook = ReadOrEmpty(hookPath);
        Ensure(repo, Path.GetRelativePath(repo, hookPath), Merge(hook.Length == 0 ? "#!/bin/sh\n" : hook, relRoot, HookBlock(relRoot, all)), changed);
        if (!OperatingSystem.IsWindows())
            File.SetUnixFileMode(hookPath, File.GetUnixFileMode(hookPath) | UnixFileMode.UserExecute | UnixFileMode.GroupExecute | UnixFileMode.OtherExecute);
        return changed;
    }

    // The git work tree holding root, or root itself outside git.
    private static string RepoTop(string root)
    {
        var (code, top) = Proc.Run("git", ["-C", root, "rev-parse", "--show-toplevel"], 20);
        return code == 0 && top.Length > 0 ? Path.GetFullPath(top) : root;
    }

    // The pre-commit file git runs: in core.hooksPath when set, otherwise .githooks/ (versioned, and switched on here); null outside git.
    private static string? HookPath(string repo)
    {
        if (!Directory.Exists(Path.Combine(repo, ".git")) && !File.Exists(Path.Combine(repo, ".git")))
            return null;
        var (code, hooksPath) = Proc.Run("git", ["-C", repo, "config", "core.hooksPath"], 20);
        if (code == 0 && hooksPath.Length > 0)
            return Path.Combine(Path.GetFullPath(hooksPath, repo), "pre-commit");
        var target = Path.Combine(repo, ".githooks", "pre-commit");
        var old = Path.Combine(repo, ".git", "hooks", "pre-commit");
        // Switching hooksPath turns off .git/hooks, so an existing hook moves along with it.
        if (!File.Exists(target) && File.Exists(old))
        {
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            File.Copy(old, target);
        }
        Proc.Run("git", ["-C", repo, "config", "core.hooksPath", ".githooks"], 20);
        return target;
    }

    // The folders of the restructured projects under root, sorted.
    private static List<string> RestructuredProjects(string root) =>
        Solution.Projects(root).Select(p => Path.GetDirectoryName(p)!).Where(Layout.IsRestructured).Distinct().Order(StringComparer.Ordinal).ToList();

    // The switches an earlier run recorded in the hook block for this root.
    private static List<string> KnownSwitches(string hookPath, string relRoot)
    {
        var block = Block(relRoot).Match(ReadOrEmpty(hookPath));
        return block.Success ? AllSwitches.Where(block.Value.Contains).ToList() : [];
    }

    // Where the skill lives: beside the repository's other skills (agents/skills, else .claude/skills, else agents/skills).
    private static string SkillRel(string repo)
    {
        var dir = !Directory.Exists(Path.Combine(repo, "agents", "skills")) && Directory.Exists(Path.Combine(repo, ".claude", "skills"))
            ? ".claude/skills" : "agents/skills";
        return $"{dir}/zazzle-csharp-minifier/SKILL.md";
    }

    // The skill text built into the binary.
    private static string SkillText()
    {
        using var stream = typeof(RepoSetup).Assembly.GetManifestResourceStream("SKILL.md")!;
        return new StreamReader(stream).ReadToEnd();
    }

    // The AGENTS.md section for one root.
    private static string AgentsSection(string relRoot, string skillRel, List<string> switches, List<string> restructured)
    {
        var where = relRoot == "." ? "" : $" in `{relRoot}`";
        var command = string.Join(" ", switches);
        var projects = string.Join(", ", restructured.Select(d => $"`{d}`"));
        var directive = restructured.Count == 0 ? "" :
            $"\n- In the restructured projects ({projects}), read `incl/` first and always; open files in `src/`, `doc/` and `test/` only by exact path when you need that file's bodies, comments or tests; never list or search those folders wholesale (their `.ignore` keeps them out of broad searches).";
        return $"""
            {Begin(relRoot, true)}
            ## C# layout{where}
            - Keep C#{where} in the layout of `zazzle-csharp-minifier {command}`: follow `{skillRel}`.
            - After changing C#, run `zazzle-csharp-minifier {command}`{(relRoot == "." ? "" : $" with `--root {relRoot}`")} and commit what it changes; the pre-commit hook runs it with `--check` and refuses violations.{directive}
            {End(relRoot, true)}

            """;
    }

    // The pre-commit block for one root: checks the staged .cs files with the recorded switches.
    private static string HookBlock(string relRoot, List<string> switches)
    {
        var command = string.Join(" ", switches);
        var pathspec = relRoot == "." ? "*.cs" : $"{relRoot}/*.cs";
        return $$"""
            {{Begin(relRoot, false)}}
            # Refuses the commit when staged C# breaks the layout zazzle-csharp-minifier keeps.
            if [ -n "$(git diff --cached --name-only --diff-filter=ACMR -- '{{pathspec}}')" ]; then
              if ! command -v zazzle-csharp-minifier >/dev/null 2>&1; then
                echo "pre-commit: zazzle-csharp-minifier is not on PATH; install it to commit C# here" >&2
                exit 1
              fi
              git diff --cached --name-only --diff-filter=ACMR -z -- '{{pathspec}}' \
                | (cd "$(git rev-parse --show-toplevel)" && xargs -0 zazzle-csharp-minifier --check --root '{{relRoot}}' {{command}}) \
                || { echo "pre-commit: run zazzle-csharp-minifier --root '{{relRoot}}' {{command}}, then commit again" >&2; exit 1; }
            fi
            {{End(relRoot, false)}}

            """;
    }

    // Replaces this root's marked block in the text, or appends it.
    private static string Merge(string text, string relRoot, string block)
    {
        var existing = Block(relRoot).Match(text);
        if (existing.Success)
            return text[..existing.Index] + block.TrimEnd('\n') + text[(existing.Index + existing.Length)..];
        var gap = text.Length == 0 ? "" : text.EndsWith("\n\n") ? "" : text.EndsWith('\n') ? "\n" : "\n\n";
        return text + gap + block;
    }

    // Writes the file when its content differs, and records the change.
    private static void Ensure(string repo, string rel, string text, List<string> changed)
    {
        var path = Path.Combine(repo, rel);
        if (ReadOrEmpty(path) == text)
            return;
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, text);
        changed.Add(rel.Replace('\\', '/'));
    }

    // The file's text, or empty when it does not exist.
    private static string ReadOrEmpty(string path) => File.Exists(path) ? File.ReadAllText(path) : "";

    // The opening marker of a root's block: an HTML comment in Markdown, a # comment in shell.
    private static string Begin(string relRoot, bool markdown) => Marker($"zazzle-csharp-minifier begin {relRoot}", markdown);

    // The closing marker of a root's block.
    private static string End(string relRoot, bool markdown) => Marker($"zazzle-csharp-minifier end {relRoot}", markdown);

    // Wraps marker text in the comment syntax of the file.
    private static string Marker(string text, bool markdown) => markdown ? $"<!-- {text} -->" : $"# {text}";

    // Matches a root's block, in either comment syntax, from its opening line to its closing line.
    private static Regex Block(string relRoot) =>
        new($"^[^\\n]*zazzle-csharp-minifier begin {Regex.Escape(relRoot)}( -->)?$.*?^[^\\n]*zazzle-csharp-minifier end {Regex.Escape(relRoot)}( -->)?$",
            RegexOptions.Singleline | RegexOptions.Multiline);
}
