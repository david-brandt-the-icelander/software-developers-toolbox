// `csharp-indexer init`: wires zazzle (the front door for everything) and the redirect hook into a project's Claude Code settings.
using System.Text.Json;
using System.Text.Json.Nodes;

namespace CSharpIndexer.Cli;

// Static installer; merges into existing JSON files and is safe to run twice.
public static class Init
{
    private const string HookMatcher = "Bash|Grep|Glob|Read";

    // Finds zazzle's daemon beside this executable (both live in libexec/zazzle), as an upgrade-proof path; null when it is not installed.
    public static string? FindDaemon(string exe)
    {
        var dir = Path.GetDirectoryName(exe);
        var daemon = string.IsNullOrEmpty(dir) ? null : Path.Combine(dir, "daemon");
        return daemon is not null && File.Exists(daemon) ? StablePath(daemon) : null;
    }

    // Maps a Homebrew keg path (PREFIX/Cellar/NAME/VERSION/...) to its opt link (PREFIX/opt/NAME/...) when that exists, so a written path survives upgrades.
    public static string StablePath(string path)
    {
        var parts = path.Split('/');
        var at = Array.IndexOf(parts, "Cellar");
        if (at < 0 || parts.Length <= at + 3)
            return path;
        var opt = string.Join('/', parts[..at].Append("opt").Append(parts[at + 1]).Concat(parts[(at + 3)..]));
        return File.Exists(opt) ? opt : path;
    }

    // Adds a name to a JSON string array once.
    private static void AddOnce(JsonArray list, string name)
    {
        if (!list.Any(a => a?.GetValue<string>() == name))
            list.Add(name);
    }

    // Removes a name from a JSON string array.
    private static void Remove(JsonArray list, string name)
    {
        foreach (var a in list.Where(a => a?.GetValue<string>() == name).ToList())
            list.Remove(a);
    }

    // Writes .mcp.json and .claude/settings.json under dir, registering zazzle when it is installed and dropping the indexer's own server, which lists no tools; returns what it did.
    public static List<string> Apply(string dir, string exe, string? daemon)
    {
        var done = new List<string>();
        var mcp = Path.Combine(dir, ".mcp.json");
        var m = Load(mcp);
        var servers = m["mcpServers"] as JsonObject ?? new JsonObject();
        servers.Remove("csharp-indexer");
        if (daemon is not null)
            servers["zazzle"] = new JsonObject { ["type"] = "stdio", ["command"] = daemon, ["args"] = new JsonArray("mcp") };
        else
            done.Add("zazzle's daemon not found beside the indexer: install zazzle, the front door for reading and searching, then run init again");
        m["mcpServers"] = servers;
        Save(mcp, m);
        done.Add($"wrote {mcp}");

        var settings = Path.Combine(dir, ".claude", "settings.json");
        var s = Load(settings);
        var hooks = s["hooks"] as JsonObject ?? new JsonObject();
        var pre = hooks["PreToolUse"] as JsonArray ?? new JsonArray();
        foreach (var existing in pre.Where(e => e?["hooks"]?.AsArray().Any(h => h?["command"]?.GetValue<string>().EndsWith(" hook") == true && h["command"]!.GetValue<string>().Contains("csharp-indexer")) == true).ToList())
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
        Remove(allow, "mcp__zazzle-csharp-indexer");
        if (daemon is not null)
            AddOnce(allow, "mcp__zazzle");
        perms["allow"] = allow;
        s["permissions"] = perms;
        var enabled = s["enabledMcpjsonServers"] as JsonArray ?? new JsonArray();
        Remove(enabled, "csharp-indexer");
        if (daemon is not null)
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
        - For C# structure, zazzle asks csharp-indexer: `zazzle_overview`, `zazzle_find_symbol`, `zazzle_refs`, `zazzle_changed_since`, `zazzle_git_log`, `zazzle_git_blame`.
        - Say what you needed and could not get with `zazzle_report_gap`.
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
