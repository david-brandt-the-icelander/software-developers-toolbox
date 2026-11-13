// Tests for the gap tool, hook policy, init and report.
using System.Text.Json.Nodes;
using ZazzleIndexer.Cli;
using ZazzleIndexer.Tools;
using Xunit;

namespace ZazzleIndexer.Tests;

// Exercises the learning-loop and setup pieces.
public class CliTests
{
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
        Assert.Contains("zazzle_find_symbol", HookPolicy.Decide("Grep", null).Message);
        Assert.DoesNotContain("find_references", HookPolicy.Decide("Grep", null).Message);
    }

    [Fact]
    // init merges into existing settings, is idempotent, keeps unrelated keys and drops the indexer's own server from an earlier init.
    public void test_init_idempotent_and_preserving()
    {
        var dir = Directory.CreateTempSubdirectory("init").FullName;
        Directory.CreateDirectory(Path.Combine(dir, ".claude"));
        File.WriteAllText(Path.Combine(dir, ".claude", "settings.json"), "{\"model\":\"x\",\"permissions\":{\"allow\":[\"Bash(ls:*)\",\"mcp__zazzle-csharp-indexer\"]},\"enabledMcpjsonServers\":[\"zazzle-csharp-indexer\"]}");
        File.WriteAllText(Path.Combine(dir, ".mcp.json"), "{\"mcpServers\":{\"zazzle-csharp-indexer\":{\"type\":\"stdio\",\"command\":\"/old\",\"args\":[\"serve\"]},\"other\":{\"command\":\"o\"}}}");
        Init.Apply(dir, "/opt/zazzle-csharp-indexer", "/opt/zazzled");
        Init.Apply(dir, "/opt/zazzle-csharp-indexer", "/opt/zazzled");
        var s = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".claude", "settings.json")))!;
        Assert.Equal("x", s["model"]!.GetValue<string>());
        Assert.Equal(["Bash(ls:*)", "mcp__zazzle"], s["permissions"]!["allow"]!.AsArray().Select(n => n!.GetValue<string>()));
        Assert.Equal(["zazzle"], s["enabledMcpjsonServers"]!.AsArray().Select(n => n!.GetValue<string>()));
        Assert.Equal("Bash|Grep|Glob|Read", s["hooks"]!["PreToolUse"]![0]!["matcher"]!.GetValue<string>());
        Assert.Single(s["hooks"]!["PreToolUse"]!.AsArray());
        Assert.Equal("/opt/zazzle-csharp-indexer hook", s["hooks"]!["PreToolUse"]![0]!["hooks"]![0]!["command"]!.GetValue<string>());
        var m = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".mcp.json")))!;
        Assert.Null(m["mcpServers"]!["zazzle-csharp-indexer"]);
        Assert.Equal("o", m["mcpServers"]!["other"]!["command"]!.GetValue<string>());
        Assert.Equal("/opt/zazzled", m["mcpServers"]!["zazzle"]!["command"]!.GetValue<string>());
        Assert.Equal("mcp", m["mcpServers"]!["zazzle"]!["args"]![0]!.GetValue<string>());
        Directory.Delete(dir, true);
    }

    [Fact]
    // Without zazzled, init still wires the hook and says how to get the front door.
    public void test_init_without_zazzled()
    {
        var dir = Directory.CreateTempSubdirectory("init").FullName;
        var done = Init.Apply(dir, "/opt/zazzle-csharp-indexer", null);
        Assert.Contains(done, l => l.StartsWith("zazzled not found"));
        var m = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".mcp.json")))!;
        Assert.Null(m["mcpServers"]!["zazzle"]);
        Assert.Empty(m["mcpServers"]!.AsObject());
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
    // report aggregates tools, resolution paths, hook blocks and gap notes, also from records of tools since removed.
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
        Assert.DoesNotContain("run passthrough", text);
        Assert.Contains("blocked:grep: 1", text);
        Assert.Contains("retries (same tool within 10 s, different arguments): 1", text);
        Assert.Contains("need call graph", text);
        Assert.DoesNotContain("search selectivity", text);
    }
}
