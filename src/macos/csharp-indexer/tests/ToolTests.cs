// Tests for the MCP dispatcher: no tools listed, internal tools called by name and logged.
using System.Text.Json.Nodes;
using CSharpIndexer.Mcp;
using Xunit;

namespace CSharpIndexer.Tests;

// Exercises the server against a temporary repository.
public class ToolTests
{
    private static readonly Dictionary<string, string> Files = new()
    {
        ["src/a.cs"] = "class Needle\n{\n}\n",
        ["docs/readme.md"] = "Needle in docs\n",
    };

    [Fact]
    // The server lists no tools and points agents at zazzle, but answers daemon's calls by name and logs them with free text hashed.
    public void test_server_lists_no_tools_and_logs()
    {
        using var repo = new TempRepo(Files);
        var logFile = Path.Combine(repo.Dir, "..", $"log-{Guid.NewGuid():N}.jsonl");
        var server = new McpServer([repo.Root()], new UsageLog(logFile), "client-session");
        var init = server.Handle("{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\"}")!;
        Assert.Contains("zazzle_find_symbol", init["result"]!["instructions"]!.GetValue<string>());
        var list = server.Handle("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}")!;
        Assert.Empty(list["result"]!["tools"]!.AsArray());
        var call = server.Handle("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"find_symbol\",\"arguments\":{\"name\":\"needle\"}}}")!;
        Assert.False(call["result"]!["isError"]!.GetValue<bool>());
        Assert.StartsWith("r:src/a.cs:1-3 class Needle", call["result"]!["content"]![0]!["text"]!.GetValue<string>());
        var record = JsonNode.Parse(File.ReadAllText(logFile))!;
        Assert.Equal(2, record["v"]!.GetValue<int>());
        Assert.Equal("client-session", record["session"]!.GetValue<string>());
        Assert.Equal("find_symbol", record["tool"]!.GetValue<string>());
        Assert.Equal(12, record["response_hash"]!.GetValue<string>().Length);
        Assert.DoesNotContain("needle", record.ToJsonString());
        File.Delete(logFile);
    }

    [Theory]
    [InlineData("search")]
    [InlineData("read")]
    [InlineData("find_files")]
    [InlineData("outline")]
    [InlineData("run")]
    [InlineData("find_references")]
    // The tools zazzle replaced are gone: calling one is an unknown-tool error.
    public void test_removed_tools_are_unknown(string name)
    {
        using var repo = new TempRepo(Files);
        var server = new McpServer([repo.Root()], new UsageLog(null));
        var call = server.Handle($"{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"{name}\",\"arguments\":{{}}}}}}")!;
        Assert.True(call["result"]!["isError"]!.GetValue<bool>());
        Assert.Equal($"unknown tool: {name}", call["result"]!["content"]![0]!["text"]!.GetValue<string>());
    }

    [Fact]
    // refs answers from Roslyn: calls through an interface, every overload and both halves of a partial class, but no comment or string.
    public void test_refs_exact()
    {
        using var repo = new TempRepo(new()
        {
            ["src/I.cs"] = "interface IShape { int Area(); }\n",
            ["src/Sq.cs"] = "partial class Sq : IShape { public int Area() => 1; }\n",
            ["src/Sq2.cs"] = "partial class Sq { int Area(int k) => k; }\n",
            ["src/Use.cs"] = "class Use {\n  // Area in a comment\n  string s = \"Area\";\n  int M(IShape x, Sq q) => x.Area() + q.Area() + q.Area(2);\n}\n",
        });
        var server = new McpServer([repo.Root()], new UsageLog(null));
        string Call(string args) => server.Handle($"{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"refs\",\"arguments\":{args}}}}}")!["result"]!["content"]![0]!["text"]!.GetValue<string>();
        var area = Call("{\"name\":\"Area\"}");
        Assert.Contains("4 int M(IShape x, Sq q) => x.Area() + q.Area() + q.Area(2);", area);
        Assert.DoesNotContain("comment", area);
        Assert.DoesNotContain("\"Area\"", area);
        Assert.Contains("-- exact (Roslyn): 3 defs, 1 refs in 1 files", area);
        var sq = Call("{\"name\":\"Sq\"}");
        Assert.Contains("r:src/Sq.cs:1 class Sq", sq);
        Assert.Contains("r:src/Sq2.cs:1 class Sq", sq);
        Assert.Contains("Sq.Area(int)", Call("{\"name\":\"Sq.Area\"}"));
        Assert.Contains("no C# declaration of Nope", Call("{\"name\":\"Nope\"}"));
    }
}
