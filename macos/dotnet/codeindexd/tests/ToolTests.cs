// Tests for the glob helper, the tools and the MCP dispatcher.
using System.Text.Json.Nodes;
using CodeIndexd.Mcp;
using CodeIndexd.Tools;
using Xunit;

namespace CodeIndexd.Tests;

// Exercises tools against a temporary repository.
public class ToolTests
{
    private static readonly Dictionary<string, string> Files = new()
    {
        ["src/a.cs"] = "one\ntwo needle\nthree\nfour\nfive needle\n",
        ["src/b.txt"] = "nothing here\n",
        ["docs/readme.md"] = "Needle in docs\n",
        [".gitignore"] = "ignored.txt\n",
    };

    // Runs a tool with JSON arguments and returns its text.
    private static string Call(ITool tool, string json, CallInfo? info = null) => tool.Run(JsonNode.Parse(json), info ?? new CallInfo());

    [Fact]
    // A glob without a slash matches file names at any depth.
    public void test_glob_name_any_depth() => Assert.Matches(Glob.ToRegex("*.cs"), "src/deep/a.cs");

    [Fact]
    // A glob with ** crosses directories but * does not.
    public void test_glob_star_stays_in_directory()
    {
        Assert.Matches(Glob.ToRegex("src/**/*.cs"), "src/x/y/a.cs");
        Assert.DoesNotMatch(Glob.ToRegex("src/*.cs"), "src/x/a.cs");
    }

    [Fact]
    // find_files hides ignored files and filters by glob.
    public void test_find_files_respects_gitignore_and_glob()
    {
        using var repo = new TempRepo(Files);
        File.WriteAllText(Path.Combine(repo.Dir, "ignored.txt"), "x");
        var tool = new FindFilesTool(new RootSet([repo.Root()]));
        var all = Call(tool, "{}");
        Assert.DoesNotContain("ignored.txt", all);
        Assert.Contains("r:src/a.cs", all);
        Assert.Equal("r:src/a.cs\n[1 files]", Call(tool, "{\"glob\":\"*.cs\"}"));
    }

    [Fact]
    // read returns numbered lines and a continuation hint when the budget is small.
    public void test_read_range_and_truncation()
    {
        using var repo = new TempRepo(Files);
        var tool = new ReadTool(new RootSet([repo.Root()]));
        Assert.StartsWith("2\ttwo needle\n3\tthree\n", Call(tool, "{\"path\":\"r:src/a.cs\",\"start_line\":2,\"end_line\":3}"));
        var info = new CallInfo();
        Call(tool, "{\"path\":\"r:src/a.cs\",\"max_tokens\":1}", info);
        Assert.False(info.Truncated);
    }

    [Fact]
    // read refuses paths that escape the root.
    public void test_read_rejects_escape()
    {
        using var repo = new TempRepo(Files);
        var tool = new ReadTool(new RootSet([repo.Root()]));
        var e = Assert.Throws<ToolException>(() => Call(tool, "{\"path\":\"r:../../etc/passwd\"}"));
        Assert.Equal("outside_root", e.Code);
    }

    [Fact]
    // search finds matches in path order with context and pages with offset.
    public void test_search_context_and_offset()
    {
        using var repo = new TempRepo(Files);
        var tool = new SearchTool(new RootSet([repo.Root()]));
        var text = Call(tool, "{\"pattern\":\"needle\",\"ignore_case\":true,\"context\":1}");
        Assert.Contains("r:docs/readme.md\n1:Needle in docs", text);
        Assert.Contains("1-one\n2:two needle\n3-three", text);
        Assert.EndsWith("[3 matches]", text);
        var paged = Call(tool, "{\"pattern\":\"needle\",\"max_results\":1,\"offset\":1}");
        Assert.Contains("5:five needle", paged);
        Assert.DoesNotContain("2:two", paged);
    }

    [Fact]
    // search reports a bad regex with the dialect notes and treats literal text literally.
    public void test_search_bad_regex_and_literal()
    {
        using var repo = new TempRepo(Files);
        var tool = new SearchTool(new RootSet([repo.Root()]));
        var e = Assert.Throws<ToolException>(() => Call(tool, "{\"pattern\":\"(\"}"));
        Assert.Equal("bad_regex", e.Code);
        Assert.Contains(".NET regex", e.Message);
        Assert.EndsWith("[0 matches]", Call(tool, "{\"pattern\":\"(\",\"literal\":true}"));
    }

    [Fact]
    // The server lists its tools and writes a log record that hides the pattern text.
    public void test_server_lists_tools_and_logs()
    {
        using var repo = new TempRepo(Files);
        var logFile = Path.Combine(repo.Dir, "..", $"log-{Guid.NewGuid():N}.jsonl");
        var server = new McpServer([repo.Root()], new UsageLog(logFile));
        var list = server.Handle("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}")!;
        Assert.Equal(8, list["result"]!["tools"]!.AsArray().Count);
        var call = server.Handle("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"pattern\":\"needle\"}}}")!;
        Assert.False(call["result"]!["isError"]!.GetValue<bool>());
        var record = File.ReadAllText(logFile);
        Assert.Contains("\"tool\":\"search\"", record);
        Assert.DoesNotContain("needle", record);
        File.Delete(logFile);
    }
}
