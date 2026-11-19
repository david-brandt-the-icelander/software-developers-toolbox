// Tests for uninit and the text-level JSON removal it uses.
using System.Text;
using System.Text.Json.Nodes;
using CSharpIndexer.Cli;
using Xunit;

namespace CSharpIndexer.Tests;

// Runs init and uninit on temporary project folders.
public class UninitTests
{
    // Writes a project folder with optional .mcp.json and .claude/settings.json text.
    private static string Project(string? mcp, string? settings)
    {
        var dir = Directory.CreateTempSubdirectory("uninit").FullName;
        if (mcp is not null)
            File.WriteAllText(Path.Combine(dir, ".mcp.json"), mcp);
        if (settings is not null)
        {
            Directory.CreateDirectory(Path.Combine(dir, ".claude"));
            File.WriteAllText(Path.Combine(dir, ".claude", "settings.json"), settings);
        }
        return dir;
    }

    [Theory]
    [InlineData("{\"a\":1,\"b\":2,\"c\":3}", "b", "{\"a\":1,\"c\":3}")]
    [InlineData("{\n  \"a\": 1,\n  \"b\": 2\n}", "a", "{\n  \"b\": 2\n}")]
    [InlineData("{\n  \"a\": 1,\n  \"b\": 2\n}", "b", "{\n  \"a\": 1\n}")]
    [InlineData("{ \"a\": {\"x\": [1]} }", "a", "{}")]
    [InlineData("{\"a\":1}", "z", null)]
    // A property is cut with its comma and the whitespace that belonged to it; nothing else moves.
    public void test_json_text_remove_property(string json, string name, string? expected)
    {
        var cut = JsonText.Remove(Encoding.UTF8.GetBytes(json), [name]);
        Assert.Equal(expected, cut is null ? null : Encoding.UTF8.GetString(cut));
    }

    [Fact]
    // Array elements are found by index, also inside nested objects, and escaped names match.
    public void test_json_text_remove_nested()
    {
        var json = Encoding.UTF8.GetBytes("{\"p\": {\"a\\u006cl\": [\"x\", \"mcp__zazzle\", \"y\"]}, \"q\": \"é\"}");
        Assert.Equal("{\"p\": {\"a\\u006cl\": [\"x\", \"y\"]}, \"q\": \"é\"}", Encoding.UTF8.GetString(JsonText.Remove(json, ["p", "all", 1])!));
        Assert.Null(JsonText.Remove(json, ["p", "all", 3]));
        Assert.Null(JsonText.Remove(json, ["q", 0]));
    }

    [Fact]
    // uninit after init on an empty project deletes the files and folder init created, and a second run finds nothing to undo.
    public void test_uninit_after_init_on_empty_project()
    {
        var dir = Project(null, null);
        Init.Apply(dir, "/opt/zazzle/libexec/zazzle/csharp-indexer", "/opt/zazzle/libexec/zazzle/daemon");
        var done = Uninit.Apply(dir, false);
        Assert.Contains($"removed the zazzle MCP server (mcpServers.zazzle) from {dir}/.mcp.json", done);
        Assert.Contains($"removed the csharp-indexer redirect hook (hooks.PreToolUse) from {dir}/.claude/settings.json", done);
        Assert.Contains($"removed the mcp__zazzle permission (permissions.allow) from {dir}/.claude/settings.json", done);
        Assert.Contains($"removed the zazzle entry of enabledMcpjsonServers from {dir}/.claude/settings.json", done);
        Assert.Contains($"deleted {dir}/.mcp.json: nothing else is left in it", done);
        Assert.Contains($"deleted the empty folder {dir}/.claude", done);
        Assert.False(File.Exists(Path.Combine(dir, ".mcp.json")));
        Assert.False(Directory.Exists(Path.Combine(dir, ".claude")));
        Assert.Equal([$"nothing to undo in {dir}"], Uninit.Apply(dir, false));
        Directory.Delete(dir, true);
    }

    [Fact]
    // uninit removes only init's items from hand-written files and keeps every other byte, including layout and other hooks.
    public void test_uninit_keeps_other_bytes()
    {
        const string mcp = "{\n    \"mcpServers\": {\n        \"other\": { \"command\": \"o\" },\n        \"zazzle\": {\"type\": \"stdio\", \"command\": \"/opt/z/daemon\", \"args\": [\"mcp\"]}\n    }\n}\n";
        const string settings = """
            {
                "model": "x",
                "hooks": {
                    "PreToolUse": [
                        { "matcher": "Bash", "hooks": [ { "type": "command", "command": "mine" } ] },
                        { "matcher": "Bash|Grep|Glob|Read", "hooks": [ { "type": "command", "command": "/opt/z/csharp-indexer hook" } ] }
                    ],
                    "Stop": [ { "hooks": [ { "type": "command", "command": "/opt/z/csharp-indexer hook" }, { "type": "command", "command": "keep" } ] } ]
                },
                "permissions": { "allow": [ "mcp__zazzle", "Bash(ls:*)" ] },
                "enabledMcpjsonServers": [ "zazzle" ]
            }
            """;
        var dir = Project(mcp, settings);
        Uninit.Apply(dir, false);
        Assert.Equal("{\n    \"mcpServers\": {\n        \"other\": { \"command\": \"o\" }\n    }\n}\n", File.ReadAllText(Path.Combine(dir, ".mcp.json")));
        Assert.Equal("""
            {
                "model": "x",
                "hooks": {
                    "PreToolUse": [
                        { "matcher": "Bash", "hooks": [ { "type": "command", "command": "mine" } ] }
                    ],
                    "Stop": [ { "hooks": [ { "type": "command", "command": "/opt/z/csharp-indexer hook" }, { "type": "command", "command": "keep" } ] } ]
                },
                "permissions": { "allow": [ "Bash(ls:*)" ] }
            }
            """, File.ReadAllText(Path.Combine(dir, ".claude", "settings.json")));
        Directory.Delete(dir, true);
    }

    [Fact]
    // A redirect hook that shares its entry with another hook is removed alone, and the entry stays.
    public void test_uninit_shared_hook_entry()
    {
        var dir = Project(null, "{\"hooks\":{\"PreToolUse\":[{\"matcher\":\"Bash\",\"hooks\":[{\"command\":\"keep\"},{\"command\":\"/x/csharp-indexer hook\"}]}]}}");
        Uninit.Apply(dir, false);
        Assert.Equal("{\"hooks\":{\"PreToolUse\":[{\"matcher\":\"Bash\",\"hooks\":[{\"command\":\"keep\"}]}]}}", File.ReadAllText(Path.Combine(dir, ".claude", "settings.json")));
        Directory.Delete(dir, true);
    }

    [Fact]
    // A zazzle server init did not write is left with a note, and a file that is not JSON is left unchanged.
    public void test_uninit_leaves_foreign_server_and_bad_json()
    {
        const string mcp = "{\"mcpServers\":{\"zazzle\":{\"type\":\"stdio\",\"command\":\"/opt/z/daemon\",\"args\":[\"mcp\"],\"env\":{\"A\":\"1\"}}}}";
        var dir = Project(mcp, "{ not json");
        var done = Uninit.Apply(dir, false);
        Assert.Contains($"left the zazzle MCP server in {dir}/.mcp.json: it is not the one init writes", done);
        Assert.Contains(done, l => l.StartsWith($"left {dir}/.claude/settings.json unchanged"));
        Assert.Equal(mcp, File.ReadAllText(Path.Combine(dir, ".mcp.json")));
        Assert.Equal("{ not json", File.ReadAllText(Path.Combine(dir, ".claude", "settings.json")));
        Directory.Delete(dir, true);
    }

    [Fact]
    // --dry-run lists the changes and writes nothing.
    public void test_uninit_dry_run()
    {
        var dir = Project(null, null);
        Init.Apply(dir, "/opt/z/csharp-indexer", "/opt/z/daemon");
        var mcp = File.ReadAllText(Path.Combine(dir, ".mcp.json"));
        var settings = File.ReadAllText(Path.Combine(dir, ".claude", "settings.json"));
        var done = Uninit.Apply(dir, true);
        Assert.Contains($"would remove the zazzle MCP server (mcpServers.zazzle) from {dir}/.mcp.json", done);
        Assert.Contains($"would delete {dir}/.claude/settings.json: nothing else is left in it", done);
        Assert.All(done, l => Assert.StartsWith("would ", l));
        Assert.Equal(mcp, File.ReadAllText(Path.Combine(dir, ".mcp.json")));
        Assert.Equal(settings, File.ReadAllText(Path.Combine(dir, ".claude", "settings.json")));
        Directory.Delete(dir, true);
    }

    [Fact]
    // init then uninit on existing settings gives back the user's own keys, and the command line takes a folder and --dry-run.
    public void test_uninit_round_trip_and_command_line()
    {
        var dir = Project("{\"mcpServers\":{\"other\":{\"command\":\"o\"}}}", "{\"model\":\"x\",\"permissions\":{\"allow\":[\"Bash(ls:*)\"]}}");
        Init.Apply(dir, "/opt/z/csharp-indexer", "/opt/z/daemon");
        var exe = ControlTests.IndexerExe();
        var dry = Proc.Run(exe, ["uninit", "--dry-run", dir], 30);
        Assert.Equal(0, dry.ExitCode);
        Assert.Contains("would remove the zazzle MCP server", dry.Stdout);
        Assert.Equal(0, Proc.Run(exe, ["uninit"], 30, dir).ExitCode);
        var s = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".claude", "settings.json")))!.AsObject();
        Assert.Equal(["model", "permissions"], s.Select(kv => kv.Key));
        Assert.Equal("Bash(ls:*)", s["permissions"]!["allow"]![0]!.GetValue<string>());
        var m = JsonNode.Parse(File.ReadAllText(Path.Combine(dir, ".mcp.json")))!;
        Assert.Equal(["other"], m["mcpServers"]!.AsObject().Select(kv => kv.Key));
        Assert.Contains("nothing to undo", Proc.Run(exe, ["uninit", dir], 30).Stdout);
        Assert.Equal(2, Proc.Run(exe, ["uninit", "a", "b"], 30).ExitCode);
        Directory.Delete(dir, true);
    }
}
