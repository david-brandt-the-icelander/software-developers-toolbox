// `zazzle-csharp-indexer init`: wires zazzle (the front door for files), the indexer and the redirect hook into a project's Claude Code settings.
using System.Text.Json;
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Cli;

// Static installer; merges into existing JSON files and is safe to run twice.
public static class Init
{
    private const string HookMatcher = "Bash|Grep|Glob|Read";

    // Finds zazzled: beside this executable (as Homebrew installs them), else on PATH; null when it is not installed.
    public static string? FindZazzled(string exe)
    {
        var dirs = new[] { Path.GetDirectoryName(exe) }.Concat((Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator));
        return dirs.Where(d => !string.IsNullOrEmpty(d)).Select(d => Path.Combine(d!, "zazzled")).FirstOrDefault(File.Exists);
    }

    // Adds a name to a JSON string array once.
    private static void AddOnce(JsonArray list, string name)
    {
        if (!list.Any(a => a?.GetValue<string>() == name))
            list.Add(name);
    }

    // Writes .mcp.json and .claude/settings.json under dir, registering zazzle when it is installed; returns what it did.
    public static List<string> Apply(string dir, string exe, string? zazzled)
    {
        var done = new List<string>();
        var mcp = Path.Combine(dir, ".mcp.json");
        var m = Load(mcp);
        var servers = m["mcpServers"] as JsonObject ?? new JsonObject();
        servers["zazzle-csharp-indexer"] = new JsonObject { ["type"] = "stdio", ["command"] = exe, ["args"] = new JsonArray("serve") };
        if (zazzled is not null)
            servers["zazzle"] = new JsonObject { ["type"] = "stdio", ["command"] = zazzled, ["args"] = new JsonArray("mcp") };
        else
            done.Add("zazzled not found beside the indexer or on PATH: install zazzle, the front door for reading and searching, then run init again");
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
        AddOnce(allow, "mcp__zazzle-csharp-indexer");
        if (zazzled is not null)
            AddOnce(allow, "mcp__zazzle");
        perms["allow"] = allow;
        s["permissions"] = perms;
        var enabled = s["enabledMcpjsonServers"] as JsonArray ?? new JsonArray();
        AddOnce(enabled, "zazzle-csharp-indexer");
        if (zazzled is not null)
            AddOnce(enabled, "zazzle");
        s["enabledMcpjsonServers"] = enabled;
        Save(settings, s);
        done.Add($"wrote {settings}");
        return done;
    }

    // The text to paste into AGENTS.md or CLAUDE.md.
    public const string AgentsSnippet = """
        ## Reading, searching and editing code
        - Use zazzle (MCP server `zazzle`) instead of grep, rg, find, cat, sed or the built-in Read for C# files: `zazzle_find`, `zazzle_read`, `zazzle_ls`, `zazzle_outline`, `zazzle_refs`, `zazzle_edit`.
        - C# files read as a terse behavioural view with source line numbers and no sha. Read with `source: true` for the code and the sha every edit needs.
        - A reply tagged `[r:ID]` comes back exactly with `zazzle_read_again`; keep ids in notes instead of content.
        - For C# structure, ask zazzle-csharp-indexer (MCP server `zazzle-csharp-indexer`): `overview`, `find_symbol`, `find_references`, `changed_since`, `git_log`, `git_blame`.
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
