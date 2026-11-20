// Tests for the gap tool, hook policy, init and report.
using System.Text.Json.Nodes;
using CSharpIndexer.Cli;
using CSharpIndexer.Tools;
using Xunit;

namespace CSharpIndexer.Tests;

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
        File.WriteAllText(Path.Combine(dir, ".claude", "settings.json"), "{\"model\":\"x\",\"permissions\":{\"allow\":[\"Bash(ls:*)\",\"mcp__zazzle-csharp-indexer\"]},\"enabledMcpjsonServers\":[\"csharp-indexer\"]}");
        File.WriteAllText(Path.Combine(dir, ".mcp.json"), "{\"mcpServers\":{\"csharp-indexer\":{\"type\":\"stdio\",\"command\":\"/old\",\"args\":[\"serve\"]},\"other\":{\"command\":\"o\"}}}");
        Init.Apply(dir, "/opt/csharp-indexer", "/opt/daemon");
        Init.Apply(dir, "/opt/csharp-indexer", "/opt/daemon");
        var s = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".claude", "settings.json")))!;
        Assert.Equal("x", s["model"]!.GetValue<string>());
        Assert.Equal(["Bash(ls:*)", "mcp__zazzle"], s["permissions"]!["allow"]!.AsArray().Select(n => n!.GetValue<string>()));
        Assert.Equal(["zazzle"], s["enabledMcpjsonServers"]!.AsArray().Select(n => n!.GetValue<string>()));
        Assert.Equal("Bash|Grep|Glob|Read", s["hooks"]!["PreToolUse"]![0]!["matcher"]!.GetValue<string>());
        Assert.Single(s["hooks"]!["PreToolUse"]!.AsArray());
        Assert.Equal("/opt/csharp-indexer hook", s["hooks"]!["PreToolUse"]![0]!["hooks"]![0]!["command"]!.GetValue<string>());
        var m = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".mcp.json")))!;
        Assert.Null(m["mcpServers"]!["csharp-indexer"]);
        Assert.Equal("o", m["mcpServers"]!["other"]!["command"]!.GetValue<string>());
        Assert.Equal("/opt/daemon", m["mcpServers"]!["zazzle"]!["command"]!.GetValue<string>());
        Assert.Equal("mcp", m["mcpServers"]!["zazzle"]!["args"]![0]!.GetValue<string>());
        Directory.Delete(dir, true);
    }

    [Fact]
    // Without daemon, init still wires the hook and says how to get the front door.
    public void test_init_without_daemon()
    {
        var dir = Directory.CreateTempSubdirectory("init").FullName;
        var done = Init.Apply(dir, "/opt/csharp-indexer", null);
        Assert.Contains(done, l => l.StartsWith("zazzle's daemon not found"));
        var m = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".mcp.json")))!;
        Assert.Null(m["mcpServers"]!["zazzle"]);
        Assert.Empty(m["mcpServers"]!.AsObject());
        Directory.Delete(dir, true);
    }

    [Fact]
    // The daemon is found only beside the indexer, and a keg path becomes its opt link when that exists.
    public void test_find_daemon()
    {
        var root = Directory.CreateTempSubdirectory("brew").FullName;
        var keg = Path.Combine(root, "Cellar/zazzle/1.2.3/libexec/zazzle");
        Directory.CreateDirectory(keg);
        File.WriteAllText(Path.Combine(keg, "daemon"), "");
        Assert.Equal(Path.Combine(keg, "daemon"), Init.FindDaemon(Path.Combine(keg, "csharp-indexer")));
        var opt = Path.Combine(root, "opt/zazzle/libexec/zazzle");
        Directory.CreateDirectory(opt);
        File.WriteAllText(Path.Combine(opt, "daemon"), "");
        Assert.Equal(Path.Combine(opt, "daemon"), Init.FindDaemon(Path.Combine(keg, "csharp-indexer")));
        Assert.Null(Init.FindDaemon("/nonexistent-dir/csharp-indexer"));
        Assert.Null(Init.FindDaemon("csharp-indexer"));
        Assert.Equal("/a/Cellar/x", Init.StablePath("/a/Cellar/x"));
        Directory.Delete(root, true);
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
            """{"v":2,"ts":"2026-11-12T10:00:09Z","session":"s1","tool":"view","args":{"path":"p"},"path":"view","reason":null,"ms":4,"bytes_out":350,"est_tokens":100,"source_est_tokens":300,"transformations":"usings=2,logging=3","results":10,"truncated":false,"error":null}""",
        };
        var text = Report.Build(lines.Select(l => JsonNode.Parse(l)!).ToList());
        Assert.Contains("5 calls in 1 sessions", text);
        Assert.Contains("search: 2, 30, 50", text);
        Assert.Contains("fallback:rg: 1 (20%)", text);
        Assert.DoesNotContain("run passthrough", text);
        Assert.Contains("blocked:grep: 1", text);
        Assert.Contains("retries (same tool within 10 s, different arguments): 1", text);
        Assert.Contains("need call graph", text);
        Assert.Contains("view compaction: ~300 source tokens to ~100 response tokens (200 saved)", text);
        Assert.Contains("usings=2,logging=3: 1", text);
        Assert.DoesNotContain("search selectivity", text);
    }

    [Fact]
    // The delivery report totals final responses and the savings from deduplicated notices.
    public void test_delivery_report()
    {
        var records = new[]
        {
            """{"session":"s1","command":"read","pre_dedupe_est_tokens":100,"delivered_est_tokens":103,"deduplicated":false}""",
            """{"session":"s1","command":"read","pre_dedupe_est_tokens":100,"delivered_est_tokens":20,"deduplicated":true}""",
            """{"session":"s2","command":"overview","pre_dedupe_est_tokens":30,"delivered_est_tokens":33,"deduplicated":false}""",
        }.Select(line => JsonNode.Parse(line)!).ToList();
        var text = Report.BuildDeliveries(records);
        Assert.Contains("3 responses in 2 sessions", text);
        Assert.Contains("~230 tokens before dedupe, ~156 delivered", text);
        Assert.Contains("deduplicated: 1, ~80 tokens avoided", text);
        Assert.Contains("read: 2, 123, 1", text);
    }
}
