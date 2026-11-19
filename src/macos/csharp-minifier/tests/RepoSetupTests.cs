// Tests for the repository setup a changing run performs: skill, AGENTS.md section and pre-commit block.
using Xunit;

namespace CSharpMinifier.Tests;

// Runs the tool's runner on throwaway git repositories.
[System.Runtime.Versioning.UnsupportedOSPlatform("windows")]
public sealed class RepoSetupTests : IDisposable
{
    private readonly string repo = Directory.CreateTempSubdirectory("csharp-minifier-repo").FullName;

    // Creates an empty git repository.
    public RepoSetupTests() => Proc.Run("git", ["-C", repo, "init", "-q"], 20);

    // Deletes the repository.
    public void Dispose() => Directory.Delete(repo, true);

    // Runs the runner with the given switches and returns its output.
    private string Run(params string[] args)
    {
        var parsed = Cli.Parse([.. args, "--root", repo]);
        var output = new StringWriter();
        Runner.Run(parsed, output, new StringWriter());
        return output.ToString();
    }

    // Reads a file in the repository.
    private string Read(string rel) => File.ReadAllText(Path.Combine(repo, rel));

    [Fact]
    // A changing run writes the skill, AGENTS.md, CLAUDE.md and an executable versioned hook, and switches hooks on.
    public void test_setup_first_run()
    {
        var output = Run("--split-tests");
        Assert.Contains("set up agents/skills/csharp-minifier/SKILL.md", output);
        Assert.StartsWith("---\nname: csharp-minifier", Read("agents/skills/csharp-minifier/SKILL.md"));
        Assert.Contains("`csharp-minifier --split-tests`", Read("AGENTS.md"));
        Assert.Equal("@AGENTS.md\n", Read("CLAUDE.md"));
        Assert.Contains("\"Bash(cat:*)\"", Read(".claude/settings.json"));
        var hook = Read(".githooks/pre-commit");
        Assert.StartsWith("#!/bin/sh\n\n# csharp-minifier begin .\n", hook);
        Assert.Contains("--check --root '.' --split-tests)", hook);
        Assert.True(File.GetUnixFileMode(Path.Combine(repo, ".githooks/pre-commit")).HasFlag(UnixFileMode.UserExecute));
        Assert.Equal(".githooks", Proc.Run("git", ["-C", repo, "config", "core.hooksPath"], 20).Output);
    }

    [Fact]
    // A later run with another switch merges both into one block per file, and an identical run changes nothing.
    public void test_setup_merges_switches()
    {
        Run("--split-tests");
        Run("--split-tests", "--allow-reads");
        Assert.Contains("--check --root '.' --allow-reads --split-tests)", Read(".githooks/pre-commit"));
        Assert.Single(System.Text.RegularExpressions.Regex.Matches(Read("AGENTS.md"), "begin \\."));
        Assert.DoesNotContain("set up", Run("--split-tests", "--allow-reads"));
    }

    [Fact]
    // Existing AGENTS.md text and an existing .git/hooks/pre-commit are kept.
    public void test_setup_keeps_existing()
    {
        File.WriteAllText(Path.Combine(repo, "AGENTS.md"), "# Rules\n- be nice\n");
        File.WriteAllText(Path.Combine(repo, ".git/hooks/pre-commit"), "#!/bin/sh\necho old\n");
        Run("--split-tests");
        Assert.StartsWith("# Rules\n- be nice\n\n<!-- csharp-minifier begin . -->\n", Read("AGENTS.md"));
        Assert.StartsWith("#!/bin/sh\necho old\n\n# csharp-minifier begin .", Read(".githooks/pre-commit"));
    }

    [Fact]
    // The read-only commands are added to .claude/settings.json, other settings and rules kept, and a second run changes nothing.
    public void test_setup_read_only_permissions()
    {
        Directory.CreateDirectory(Path.Combine(repo, ".claude"));
        File.WriteAllText(Path.Combine(repo, ".claude/settings.json"), "{\"model\":\"x\",\"permissions\":{\"allow\":[\"Bash(ls:*)\",\"Read\"],\"deny\":[\"Bash(rm:*)\"]}}");
        Run("--split-tests");
        var settings = System.Text.Json.Nodes.JsonNode.Parse(Read(".claude/settings.json"))!;
        Assert.Equal("x", settings["model"]!.GetValue<string>());
        Assert.Equal(["Bash(ls:*)", "Read", "Bash(cat:*)", "Bash(head:*)", "Bash(tail:*)", "Bash(wc:*)", "Bash(grep:*)", "Bash(rg:*)"],
            settings["permissions"]!["allow"]!.AsArray().Select(a => a!.GetValue<string>()));
        Assert.Equal("Bash(rm:*)", settings["permissions"]!["deny"]![0]!.GetValue<string>());
        Assert.DoesNotContain("find", Read(".claude/settings.json"));
        Assert.DoesNotContain(".claude/settings.json", Run("--split-tests"));
    }

    [Fact]
    // --allow-reads alone writes only .claude/settings.json, --check flags it until then, and a second run changes nothing.
    public void test_allow_reads_alone()
    {
        Assert.Contains("--allow-reads would change .claude/settings.json", Run("--allow-reads", "--check"));
        var output = Run("--allow-reads");
        Assert.Contains(".claude/settings.json: allowed the read-only commands ls, cat, head, tail, wc, grep, rg", output);
        Assert.Contains("\"Bash(rg:*)\"", Read(".claude/settings.json"));
        Assert.False(File.Exists(Path.Combine(repo, "AGENTS.md")));
        Assert.False(File.Exists(Path.Combine(repo, ".githooks/pre-commit")));
        Assert.Equal("", Run("--allow-reads").Trim());
    }

    [Fact]
    // A settings file that is not JSON is left as it was.
    public void test_setup_leaves_broken_settings()
    {
        Assert.Equal("not json", RepoSetup.ReadOnlySettings("not json"));
    }

    [Fact]
    // --check sets nothing up.
    public void test_setup_not_on_check()
    {
        Run("--split-tests", "--check");
        Assert.False(File.Exists(Path.Combine(repo, "AGENTS.md")));
    }

    [Fact]
    // The installed hook refuses a commit with a violation and allows it once the tool has run.
    public void test_setup_hook_blocks_violation()
    {
        var bin = BuildShim();
        Run("--split-tests");
        File.WriteAllText(Path.Combine(repo, "A.cs"), "// File.\npublic class A { public static void Go() { } }\n");
        File.WriteAllText(Path.Combine(repo, "ATests.cs"),
            "using Xunit;\npublic class ATests\n{\n    [Fact]\n    public void test_go()\n    {\n        // When\n        A.Go();\n        // Then\n    }\n}\n");
        Git("add", "-A");
        var env = $"PATH={bin}:{Environment.GetEnvironmentVariable("PATH")}";
        Assert.NotEqual(0, Proc.Run("env", [env, "git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"], 120).Code);
        Run("--split-tests");
        Assert.True(File.Exists(Path.Combine(repo, "A/Go.cs")));
        Assert.StartsWith("// File.", Read("A.cs"));
        Git("add", "-A");
        Assert.Equal(0, Proc.Run("env", [env, "git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x"], 120).Code);
    }

    // Runs git in the repository.
    private void Git(params string[] args) => Proc.Run("git", ["-C", repo, .. args], 20);

    // A folder with a csharp-minifier script that runs the tool's own build output, standing in for the installed binary.
    private static string BuildShim()
    {
        var srcBin = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../src/bin"));
        var app = Directory.EnumerateFiles(srcBin, "csharp-minifier", SearchOption.AllDirectories).OrderByDescending(File.GetLastWriteTimeUtc).First();
        var bin = Directory.CreateTempSubdirectory("csharp-minifier-bin").FullName;
        var shim = Path.Combine(bin, "csharp-minifier");
        File.WriteAllText(shim, $"#!/bin/sh\nexec '{app}' \"$@\"\n");
        File.SetUnixFileMode(shim, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        return bin;
    }

    // Refreshing repository setup removes obsolete commands from existing hook and guidance blocks.
    [Fact]
    public void test_setup_drops_retired_commands()
    {
        Directory.CreateDirectory(Path.Combine(repo, ".githooks"));
        File.WriteAllText(Path.Combine(repo, ".githooks/pre-commit"),
            "#!/bin/sh\n# csharp-minifier begin .\ncsharp-minifier --strip-comments --split-headers --restructure --split-tests\n# csharp-minifier end .\n");
        File.WriteAllText(Path.Combine(repo, "AGENTS.md"),
            "# Keep\n<!-- csharp-minifier begin . -->\nOld layout rules.\n<!-- csharp-minifier end . -->\n");
        File.WriteAllText(Path.Combine(repo, ".ignore"), "keep\n");
        Git("config", "core.hooksPath", ".githooks");
        Run("--split-tests");
        Assert.StartsWith("# Keep\n", Read("AGENTS.md"));
        Assert.DoesNotContain("Old layout rules.", Read("AGENTS.md"));
        foreach (var command in new[] { "--strip-comments", "--split-headers", "--restructure" })
        {
            Assert.DoesNotContain(command, Read(".githooks/pre-commit"));
            Assert.DoesNotContain(command, Read("AGENTS.md"));
            Assert.DoesNotContain(command, Read("agents/skills/csharp-minifier/SKILL.md"));
        }
        Assert.Contains("--check --root '.' --split-tests)", Read(".githooks/pre-commit"));
        Assert.Equal("keep\n", Read(".ignore"));
        Assert.DoesNotContain("set up", Run("--split-tests"));
    }
}
