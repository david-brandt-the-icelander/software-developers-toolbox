// Tests for the run policy, the run and gap tools, the external regex fallback, hook policy, init and report.
using System.Text.Json.Nodes;
using ZazzleIndexer.Cli;
using ZazzleIndexer.Tools;
using Xunit;

namespace ZazzleIndexer.Tests;

// Exercises the fallback and learning-loop pieces.
public class CliTests
{
    // Runs a tool with JSON arguments.
    private static string Call(ITool tool, string json, CallInfo? info = null) => tool.Run(JsonNode.Parse(json), info ?? new CallInfo());

    [Theory]
    [InlineData("git log -5", null)]
    [InlineData("grep -rn foo .", null)]
    [InlineData("sed -n 1,5p file.txt", null)]
    [InlineData("sed -i s/a/b/ file.txt", "sed -i would edit files")]
    [InlineData("sed s/a/b/w out.txt in.txt", "sed w/e commands are not allowed")]
    [InlineData("find . -name x -delete", "find actions that modify or run commands are not allowed")]
    [InlineData("rm -rf .", "rm is not in the read-only allowlist")]
    [InlineData("git push", "git push is not a read-only subcommand")]
    [InlineData("git -c core.pager=x log", "git -c and --output are not allowed")]
    // The default policy allows read-only commands and refuses editing or running ones.
    public void test_run_policy(string command, string? expected)
    {
        var reason = RunPolicy.Check(command.Split(' '), false);
        Assert.Equal(expected is null, reason is null);
        if (expected is not null)
            Assert.StartsWith(expected, reason);
    }

    [Fact]
    // The unrestricted switch lifts the allowlist.
    public void test_run_policy_unrestricted() => Assert.Null(RunPolicy.Check(["rm", "-rf", "x"], true));

    [Fact]
    // run executes in the root, caps output, hints at the better tool, and refuses disallowed commands.
    public void test_run_tool()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "alpha\nbeta\n" });
        var tool = new RunTool(new RootSet([repo.Root()]));
        var info = new CallInfo();
        var text = Call(tool, "{\"argv\":[\"grep\",\"-n\",\"beta\",\"a.txt\"]}", info);
        Assert.StartsWith("2:beta", text);
        Assert.Contains("[hint:", text);
        Assert.Equal("run:grep", info.Path);
        var e = Assert.Throws<ToolException>(() => Call(tool, "{\"argv\":[\"rm\",\"a.txt\"]}"));
        Assert.Equal("run_denied", e.Code);
        Assert.StartsWith("a.txt", Call(tool, "{\"argv\":[\"git\",\"ls-files\"]}"));
    }

    [Fact]
    // A pattern .NET rejects (possessive quantifier) is answered by an external PCRE engine when one is installed.
    public void test_search_falls_back_to_external_engine()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "aaab\nzzz\n" });
        var tool = new SearchTool(new RootSet([repo.Root()]));
        var info = new CallInfo();
        string text;
        try
        {
            text = Call(tool, "{\"pattern\":\"a++b\"}", info);
        }
        catch (ToolException e)
        {
            Assert.Equal("bad_regex", e.Code);
            return;
        }
        Assert.Contains("1:aaab", text);
        Assert.StartsWith("fallback:", info.Path);
        Assert.Equal("regex_unsupported", info.Reason);
    }

    [Fact]
    // report_gap stores the note in the usage log in plain text.
    public void test_report_gap_logged()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "x\n" });
        var logFile = Path.Combine(repo.Dir, "..", $"gap-{Guid.NewGuid():N}.jsonl");
        var server = new Mcp.McpServer([repo.Root()], new UsageLog(logFile));
        server.Handle("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"report_gap\",\"arguments\":{\"what\":\"need call graph\"}}}");
        Assert.Contains("\"note\":\"need call graph\"", File.ReadAllText(logFile));
        File.Delete(logFile);
    }

    [Theory]
    [InlineData("Bash", "grep -rn foo src", true)]
    [InlineData("Bash", "cd x && rg foo | head", true)]
    [InlineData("Bash", "git grep foo", true)]
    [InlineData("Bash", "find . -name '*.cs'", true)]
    [InlineData("Bash", "cat src/a.cs", true)]
    [InlineData("Bash", "sed -n '1,5p' a.cs", true)]
    [InlineData("Bash", "FOO=1 xargs grep x", true)]
    [InlineData("Bash", "ls | grep cs", true)]
    [InlineData("Bash", "dotnet build", false)]
    [InlineData("Bash", "git log -3", false)]
    [InlineData("Bash", "cat <<EOF > out.txt\nhi\nEOF", false)]
    [InlineData("Bash", "echo grep", false)]
    [InlineData("Bash", "sed -i s/a/b/ f.txt", false)]
    [InlineData("Bash", "tail -f app.log", false)]
    [InlineData("Bash", "find . -name x -exec rm {} ;", false)]
    [InlineData("Bash", "grep foo x # allow-raw", false)]
    [InlineData("Grep", null, true)]
    [InlineData("Glob", null, true)]
    [InlineData("Read", null, false)]
    // The hook blocks searching and reading commands and lets building, editing and raw-marked commands through.
    public void test_hook_policy(string tool, string? command, bool block) =>
        Assert.Equal(block, HookPolicy.Decide(tool, command).Block);

    [Theory]
    [InlineData("/w/src/Invoice.cs", true)]
    [InlineData("/w/src/Invoice.CS", true)]
    [InlineData("/w/README.md", false)]
    [InlineData(null, false)]
    // The built-in Read is sent to zazzle for C# files only, so they come as the terse view.
    public void test_hook_read_tool(string? path, bool block)
    {
        var d = HookPolicy.Decide("Read", null, path);
        Assert.Equal(block, d.Block);
        if (block)
            Assert.Contains("zazzle read", d.Message);
    }

    [Fact]
    // Blocked commands point at zazzle, not at the indexer's own read and search tools.
    public void test_hook_points_at_zazzle()
    {
        Assert.Contains("zazzle_find", HookPolicy.Decide("Bash", "grep -rn foo src").Message);
        Assert.Contains("zazzle_ls", HookPolicy.Decide("Glob", null).Message);
        Assert.Contains("zazzle_read", HookPolicy.Decide("Bash", "cat a.cs").Message);
        Assert.DoesNotContain("report_gap", HookPolicy.Decide("Bash", "cat a.cs").Message);
    }

    [Fact]
    // init merges into existing settings, is idempotent and keeps unrelated keys.
    public void test_init_idempotent_and_preserving()
    {
        var dir = Directory.CreateTempSubdirectory("init").FullName;
        Directory.CreateDirectory(Path.Combine(dir, ".claude"));
        File.WriteAllText(Path.Combine(dir, ".claude", "settings.json"), "{\"model\":\"x\",\"permissions\":{\"allow\":[\"Bash(ls:*)\"]}}");
        Init.Apply(dir, "/opt/zazzle-csharp-indexer", "/opt/zazzled");
        Init.Apply(dir, "/opt/zazzle-csharp-indexer", "/opt/zazzled");
        var s = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".claude", "settings.json")))!;
        Assert.Equal("x", s["model"]!.GetValue<string>());
        Assert.Equal(3, s["permissions"]!["allow"]!.AsArray().Count);
        Assert.Equal(["zazzle-csharp-indexer", "zazzle"], s["enabledMcpjsonServers"]!.AsArray().Select(n => n!.GetValue<string>()));
        Assert.Equal("Bash|Grep|Glob|Read", s["hooks"]!["PreToolUse"]![0]!["matcher"]!.GetValue<string>());
        Assert.Single(s["hooks"]!["PreToolUse"]!.AsArray());
        Assert.Equal("/opt/zazzle-csharp-indexer hook", s["hooks"]!["PreToolUse"]![0]!["hooks"]![0]!["command"]!.GetValue<string>());
        var m = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".mcp.json")))!;
        Assert.Equal("/opt/zazzle-csharp-indexer", m["mcpServers"]!["zazzle-csharp-indexer"]!["command"]!.GetValue<string>());
        Assert.Equal("/opt/zazzled", m["mcpServers"]!["zazzle"]!["command"]!.GetValue<string>());
        Assert.Equal("mcp", m["mcpServers"]!["zazzle"]!["args"]![0]!.GetValue<string>());
        Directory.Delete(dir, true);
    }

    [Fact]
    // Without zazzled, init still wires the indexer and says how to get the front door.
    public void test_init_without_zazzled()
    {
        var dir = Directory.CreateTempSubdirectory("init").FullName;
        var done = Init.Apply(dir, "/opt/zazzle-csharp-indexer", null);
        Assert.Contains(done, l => l.StartsWith("zazzled not found"));
        var m = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".mcp.json")))!;
        Assert.Null(m["mcpServers"]!["zazzle"]);
        Directory.Delete(dir, true);
    }

    [Fact]
    // zazzled is found beside the indexer first, else on PATH.
    public void test_find_zazzled()
    {
        var dir = Directory.CreateTempSubdirectory("bin").FullName;
        File.WriteAllText(Path.Combine(dir, "zazzled"), "");
        Assert.Equal(Path.Combine(dir, "zazzled"), Init.FindZazzled(Path.Combine(dir, "zazzle-csharp-indexer")));
        Assert.False(Init.FindZazzled("/nonexistent-dir/zazzle-csharp-indexer")?.StartsWith("/nonexistent-dir") ?? false);
        Directory.Delete(dir, true);
    }

    [Fact]
    // report aggregates tools, resolution paths, run patterns, hook blocks and gap notes.
    public void test_report()
    {
        var lines = new[]
        {
            """{"v":1,"ts":"2026-11-12T10:00:00Z","session":"s1","tool":"search","args":{"pattern":"aa"},"path":"index","reason":null,"ms":10,"bytes_out":400,"est_tokens":100,"results":3,"truncated":false,"candidates":2,"files":100,"error":null}""",
            """{"v":1,"ts":"2026-11-12T10:00:05Z","session":"s1","tool":"search","args":{"pattern":"bb"},"path":"fallback:rg","reason":"regex_unsupported","ms":50,"bytes_out":40,"est_tokens":10,"results":0,"truncated":false,"candidates":-1,"files":-1,"error":null}""",
            """{"v":1,"ts":"2026-11-12T10:00:06Z","session":"s1","tool":"run","args":{"argv":["awk","-F","aa"]},"path":"run:awk","reason":"passthrough","ms":5,"bytes_out":40,"est_tokens":10,"results":0,"truncated":false,"candidates":-1,"files":-1,"error":null}""",
            """{"v":1,"ts":"2026-11-12T10:00:07Z","session":"s1","tool":"report_gap","args":{"what":"x"},"path":"index","reason":"gap_report","ms":1,"bytes_out":8,"est_tokens":2,"results":0,"truncated":false,"note":"need call graph","error":null}""",
            """{"v":1,"ts":"2026-11-12T10:00:08Z","session":"s1","tool":"hook","args":{"tool":"Bash","program":"grep"},"path":"hook","reason":"blocked:grep","ms":0,"bytes_out":0,"est_tokens":0,"results":0,"truncated":false,"error":null}""",
        };
        var text = Report.Build(lines.Select(l => JsonNode.Parse(l)!).ToList());
        Assert.Contains("4 calls in 1 sessions", text);
        Assert.Contains("search: 2, 30, 50", text);
        Assert.Contains("fallback:rg: 1 (25%)", text);
        Assert.Contains("awk -F: 1", text);
        Assert.Contains("blocked:grep: 1", text);
        Assert.Contains("retries (same tool within 10 s, different arguments): 1", text);
        Assert.Contains("need call graph", text);
        Assert.Contains("kept on average 2.0% of files", text);
    }
}
