// `zazzle-csharp-indexer init`: wires the zazzled MCP server (which relays this indexer) and the redirect hook into a project's Claude Code settings.
using System.Text.Json;
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Cli;

// Static installer; merges into existing JSON files and is safe to run twice.
public static class Init
{
    private const string HookMatcher = "Bash|Grep|Glob";

    // Writes .mcp.json and .claude/settings.json under dir; returns what it did, one line per file.
    public static List<string> Apply(string dir, string exe)
    {
        var done = new List<string>();
        var mcp = Path.Combine(dir, ".mcp.json");
        var m = Load(mcp);
        var servers = m["mcpServers"] as JsonObject ?? new JsonObject();
        servers["zazzle"] = new JsonObject { ["type"] = "stdio", ["command"] = "zazzled", ["args"] = new JsonArray("mcp") };
        m["mcpServers"] = servers;
        Save(mcp, m);
        done.Add($"wrote {mcp}");

        var settings = Path.Combine(dir, ".claude", "settings.json");
        var s = Load(settings);
        var hooks = s["hooks"] as JsonObject ?? new JsonObject();
        var pre = hooks["PreToolUse"] as JsonArray ?? new JsonArray();
        foreach (var existing in pre.Where(e => e?["hooks"]?.AsArray().Any(h => h?["command"]?.GetValue<string>().EndsWith(" hook") == true && h["command"]!.GetValue<string>().Contains("zazzle-csharp-indexer")) == true).ToList())
            pre.Remove(existing);
        pre.Add(new JsonObject
        {
            ["matcher"] = HookMatcher,
            ["hooks"] = new JsonArray(new JsonObject { ["type"] = "command", ["command"] = $"{exe} hook" }),
        });
        hooks["PreToolUse"] = pre;
        s["hooks"] = hooks;
        var perms = s["permissions"] as JsonObject ?? new JsonObject();
        var allow = perms["allow"] as JsonArray ?? new JsonArray();
        if (!allow.Any(a => a?.GetValue<string>() == "mcp__zazzle"))
            allow.Add("mcp__zazzle");
        perms["allow"] = allow;
        s["permissions"] = perms;
        var enabled = s["enabledMcpjsonServers"] as JsonArray ?? new JsonArray();
        if (!enabled.Any(a => a?.GetValue<string>() == "zazzle"))
            enabled.Add("zazzle");
        s["enabledMcpjsonServers"] = enabled;
        Save(settings, s);
        done.Add($"wrote {settings}");
        return done;
    }

    // The text to paste into AGENTS.md or CLAUDE.md.
    public const string AgentsSnippet = """
        ## Code questions
        - Ask the MCP server `zazzle` instead of grep, rg, find, cat or sed. `zazzle_*` tools read, search, list and edit files; `index_*` tools answer code questions: `index_overview`, `index_outline`, `index_find_symbol`, `index_find_references`, `index_search`, `index_read`, `index_find_files`, `index_changed_since`.
        - `index_*` paths are `root:relative/path`. `index_search` uses .NET regex; use `literal` for plain text.
        - If the tools cannot answer, call `index_report_gap` with one sentence, then use `index_run`.
        """;

    // Reads a JSON object file, or an empty object when it does not exist.
    private static JsonObject Load(string path) =>
        File.Exists(path) ? JsonNode.Parse(File.ReadAllText(path)) as JsonObject ?? new JsonObject() : new JsonObject();

    // Writes indented JSON, creating the directory.
    private static void Save(string path, JsonObject node)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        using var stream = new MemoryStream();
        using (var writer = new Utf8JsonWriter(stream, new JsonWriterOptions { Indented = true }))
            node.WriteTo(writer);
        File.WriteAllText(path, System.Text.Encoding.UTF8.GetString(stream.ToArray()) + "\n");
    }
}
