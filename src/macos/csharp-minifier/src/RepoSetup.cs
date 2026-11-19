// Makes the repository the tool just changed keep that layout: the skill, an AGENTS.md section, read-only command permissions and a pre-commit check.
using System.Text.RegularExpressions;

namespace CSharpMinifier;

// Writes the skill, the AGENTS.md section and the pre-commit block for one root, merging switches from earlier runs.
internal static class RepoSetup
{
    // The commands in the order they run.
    private static readonly string[] AllSwitches = ["--allow-reads", "--split-tests"];

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
        Ensure(repo, "AGENTS.md", Merge(ReadOrEmpty(Path.Combine(repo, "AGENTS.md")), relRoot, AgentsSection(relRoot, skillRel, all)), changed);
        if (!File.Exists(Path.Combine(repo, "CLAUDE.md")))
            Ensure(repo, "CLAUDE.md", "@AGENTS.md\n", changed);
        Ensure(repo, ".claude/settings.json", ReadOnlySettings(ReadOrEmpty(Path.Combine(repo, ".claude", "settings.json"))), changed);
        if (hookPath == null)
            return changed;
        var hook = ReadOrEmpty(hookPath);
        Ensure(repo, Path.GetRelativePath(repo, hookPath), Merge(hook.Length == 0 ? "#!/bin/sh\n" : hook, relRoot, HookBlock(relRoot, all)), changed);
        if (!OperatingSystem.IsWindows())
            File.SetUnixFileMode(hookPath, File.GetUnixFileMode(hookPath) | UnixFileMode.UserExecute | UnixFileMode.GroupExecute | UnixFileMode.OtherExecute);
        return changed;
    }

    // Read-only shell commands agents may run here without asking; find is left out because -exec and -delete make it anything but read-only.
    public static readonly string[] ReadOnlyCommands = ["ls", "cat", "head", "tail", "wc", "grep", "rg"];

    // The repository's .claude/settings.json with the read-only Bash rules added to permissions.allow, keeping everything else; unchanged text when they are all there or the file is not a JSON object.
    public static string ReadOnlySettings(string text)
    {
        System.Text.Json.Nodes.JsonObject settings;
        try
        {
            settings = text.Trim().Length == 0 ? [] : System.Text.Json.Nodes.JsonNode.Parse(text) as System.Text.Json.Nodes.JsonObject ?? throw new System.Text.Json.JsonException();
        }
        catch (System.Text.Json.JsonException)
        {
            return text;
        }
        var permissions = settings["permissions"] as System.Text.Json.Nodes.JsonObject ?? [];
        settings["permissions"] = permissions;
        var allow = permissions["allow"] as System.Text.Json.Nodes.JsonArray ?? [];
        permissions["allow"] = allow;
        var have = allow.Select(a => a?.ToString()).ToHashSet();
        var missing = ReadOnlyCommands.Select(c => $"Bash({c}:*)").Where(r => !have.Contains(r)).ToList();
        if (missing.Count == 0 && text.Trim().Length > 0)
            return text;
        missing.ForEach(r => allow.Add(r));
        using var buffer = new MemoryStream();
        using (var writer = new System.Text.Json.Utf8JsonWriter(buffer, new System.Text.Json.JsonWriterOptions { Indented = true }))
            settings.WriteTo(writer);
        return System.Text.Encoding.UTF8.GetString(buffer.ToArray()) + "\n";
    }

    // The --allow-reads command: a plan that adds the read-only rules to the repository's .claude/settings.json, with no change when they are there.
    public static Plan AllowReads(string root)
    {
        var plan = new Plan();
        var path = Path.Combine(RepoTop(root), ".claude", "settings.json");
        var text = ReadOrEmpty(path);
        var updated = ReadOnlySettings(text);
        if (updated == text)
        {
            if (text.Trim().Length > 0 && !text.Contains("Bash(cat:*)", StringComparison.Ordinal))
                plan.Errors.Add($"{path}: left unchanged: not a JSON object");
            return plan;
        }
        plan.Write(path, updated);
        plan.Changes.Add($"{Path.GetRelativePath(root, path).Replace('\\', '/')}: allowed the read-only commands {string.Join(", ", ReadOnlyCommands)}");
        return plan;
    }

    // The git work tree holding root, or root itself outside git, spelled from root so symlinked paths (macOS /var, /tmp) still relate.
    private static string RepoTop(string root)
    {
        var (code, up) = Proc.Run("git", ["-C", root, "rev-parse", "--show-cdup"], 20);
        return code == 0 ? Path.GetFullPath(Path.Combine(root, up)).TrimEnd(Path.DirectorySeparatorChar) : root;
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
        return $"{dir}/csharp-minifier/SKILL.md";
    }

    // The skill text built into the binary.
    private static string SkillText()
    {
        using var stream = typeof(RepoSetup).Assembly.GetManifestResourceStream("SKILL.md")!;
        return new StreamReader(stream).ReadToEnd();
    }

    // The AGENTS.md section for one root.
    private static string AgentsSection(string relRoot, string skillRel, List<string> switches)
    {
        var where = relRoot == "." ? "" : $" in `{relRoot}`";
        var command = string.Join(" ", switches);
        return $"""
            {Begin(relRoot, true)}
            ## C# layout{where}
            - For C# edits{where}, follow `{skillRel}` and keep the `csharp-minifier {command}` layout.
            - Use available read/search tools and discover only the tool names needed. Request symbols or bounded ranges; avoid whole-file reads and `full:true` unless the task requires them. With zazzle, `glob` is a list of strings and `max` caps matches.
            - After changing C#, run `csharp-minifier {command}`{(relRoot == "." ? "" : $" with `--root {relRoot}`")} and commit what it changes; the pre-commit hook runs it with `--check` and refuses violations.
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
            # Refuses the commit when staged C# breaks the layout csharp-minifier keeps.
            if [ -n "$(git diff --cached --name-only --diff-filter=ACMR -- '{{pathspec}}')" ]; then
              if ! command -v csharp-minifier >/dev/null 2>&1; then
                echo "pre-commit: csharp-minifier is not on PATH; install it to commit C# here" >&2
                exit 1
              fi
              git diff --cached --name-only --diff-filter=ACMR -z -- '{{pathspec}}' \
                | (cd "$(git rev-parse --show-toplevel)" && xargs -0 csharp-minifier --check --root '{{relRoot}}' {{command}}) \
                || { echo "pre-commit: run csharp-minifier --root '{{relRoot}}' {{command}}, then commit again" >&2; exit 1; }
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
    private static string Begin(string relRoot, bool markdown) => Marker($"csharp-minifier begin {relRoot}", markdown);

    // The closing marker of a root's block.
    private static string End(string relRoot, bool markdown) => Marker($"csharp-minifier end {relRoot}", markdown);

    // Wraps marker text in the comment syntax of the file.
    private static string Marker(string text, bool markdown) => markdown ? $"<!-- {text} -->" : $"# {text}";

    // Matches a root's block, in either comment syntax, from its opening line to its closing line.
    private static Regex Block(string relRoot) =>
        new($"^[^\\n]*csharp-minifier begin {Regex.Escape(relRoot)}( -->)?$.*?^[^\\n]*csharp-minifier end {Regex.Escape(relRoot)}( -->)?$",
            RegexOptions.Singleline | RegexOptions.Multiline);
}
