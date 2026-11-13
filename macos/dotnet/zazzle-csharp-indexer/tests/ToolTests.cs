// Tests for the MCP dispatcher: no tools listed, internal tools called by name and logged.
using ZazzleIndexer.Mcp;
using Xunit;

namespace ZazzleIndexer.Tests;

// Exercises the server against a temporary repository.
public class ToolTests
{
    private static readonly Dictionary<string, string> Files = new()
    {
        ["src/a.cs"] = "class Needle\n{\n}\n",
        ["docs/readme.md"] = "Needle in docs\n",
    };

    [Fact]
    // The server lists no tools and points agents at zazzle, but answers zazzled's calls by name and logs them with free text hashed.
    public void test_server_lists_no_tools_and_logs()
    {
        using var repo = new TempRepo(Files);
        var logFile = Path.Combine(repo.Dir, "..", $"log-{Guid.NewGuid():N}.jsonl");
        var server = new McpServer([repo.Root()], new UsageLog(logFile));
        var init = server.Handle("{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\"}")!;
        Assert.Contains("zazzle_find_symbol", init["result"]!["instructions"]!.GetValue<string>());
        var list = server.Handle("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}")!;
        Assert.Empty(list["result"]!["tools"]!.AsArray());
        var call = server.Handle("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"find_symbol\",\"arguments\":{\"name\":\"needle\"}}}")!;
        Assert.False(call["result"]!["isError"]!.GetValue<bool>());
        Assert.StartsWith("r:src/a.cs:1 class Needle", call["result"]!["content"]![0]!["text"]!.GetValue<string>());
        var record = File.ReadAllText(logFile);
        Assert.Contains("\"tool\":\"find_symbol\"", record);
        Assert.DoesNotContain("needle", record);
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
}
