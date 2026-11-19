// `csharp-indexer uninit`: removes what init wrote into a project's .mcp.json and .claude/settings.json and leaves every other byte alone.
using System.Text.Json;
using System.Text.Json.Nodes;

namespace CSharpIndexer.Cli;

// Static undo of init; each removal is cut out of the file text, so it is safe to run twice and keeps the user's layout.
public static class Uninit
{
    private static readonly byte[] Bom = [0xEF, 0xBB, 0xBF];

    // Undoes init in dir (or says what it would undo, with dryRun) and returns one line per change.
    public static List<string> Apply(string dir, bool dryRun)
    {
        var done = new List<string>();
        var mcp = Path.Combine(dir, ".mcp.json");
        Undo(mcp, NextMcp, dryRun, done);
        if (Read(mcp) is { } m && (m["mcpServers"] as JsonObject)?["zazzle"] is JsonObject z && !InitServer(z))
            done.Add($"left the zazzle MCP server in {mcp}: it is not the one init writes");
        var claude = Path.Combine(dir, ".claude");
        var deleted = Undo(Path.Combine(claude, "settings.json"), NextSettings, dryRun, done);
        if (deleted && !dryRun && Directory.Exists(claude) && !Directory.EnumerateFileSystemEntries(claude).Any())
        {
            Directory.Delete(claude);
            done.Add($"deleted the empty folder {claude}");
        }
        if (done.Count == 0)
            done.Add($"nothing to undo in {dir}");
        return done;
    }

    // Removes init's items from one file, one at a time, then the containers that removal left empty; deletes the file when nothing meaningful is left. Returns true when the file is (or would be) deleted.
    private static bool Undo(string file, Func<JsonObject, (object[] Path, string What)?> next, bool dryRun, List<string> done)
    {
        if (!File.Exists(file))
            return false;
        var raw = File.ReadAllBytes(file);
        var bom = raw.AsSpan().StartsWith(Bom);
        var json = bom ? raw[Bom.Length..] : raw;
        var changes = new List<string>();
        try
        {
            while (JsonNode.Parse(json) is JsonObject root && next(root) is { } edit && JsonText.Remove(json, edit.Path) is { } cut)
            {
                changes.Add(edit.What);
                json = Prune(cut, edit.Path[..^1], changes);
            }
        }
        catch (Exception e) when (e is JsonException or ArgumentException or InvalidOperationException)
        {
            done.Add($"left {file} unchanged: it is not JSON this tool can edit ({e.Message})");
            return false;
        }
        if (changes.Count == 0)
            return false;
        done.AddRange(changes.Select(c => $"{(dryRun ? "would remove" : "removed")} {c} from {file}"));
        if (Hollow(JsonNode.Parse(json)))
        {
            if (!dryRun)
                File.Delete(file);
            done.Add($"{(dryRun ? "would delete" : "deleted")} {file}: nothing else is left in it");
            return true;
        }
        if (!dryRun)
            File.WriteAllBytes(file, bom ? [.. Bom, .. json] : json);
        return false;
    }

    // Removes the containers on path that a removal left empty, innermost first, noting each.
    private static byte[] Prune(byte[] json, object[] path, List<string> changes)
    {
        while (path.Length > 0 && At(JsonNode.Parse(json), path) is JsonObject { Count: 0 } or JsonArray { Count: 0 } && JsonText.Remove(json, path) is { } cut)
        {
            changes.Add($"the now-empty {Dotted(path)}");
            json = cut;
            path = path[..^1];
        }
        return json;
    }

    // Finds init's next item in .mcp.json: its zazzle server.
    private static (object[] Path, string What)? NextMcp(JsonObject root) =>
        (root["mcpServers"] as JsonObject)?["zazzle"] is JsonObject z && InitServer(z) ? (["mcpServers", "zazzle"], "the zazzle MCP server (mcpServers.zazzle)") : null;

    // Finds init's next item in .claude/settings.json: the redirect hook, the mcp__zazzle permission or the zazzle server switch.
    private static (object[] Path, string What)? NextSettings(JsonObject root)
    {
        if ((root["hooks"] as JsonObject)?["PreToolUse"] is JsonArray pre)
            for (var i = 0; i < pre.Count; i++)
            {
                if ((pre[i] as JsonObject)?["hooks"] is not JsonArray hooks)
                    continue;
                var ours = Enumerable.Range(0, hooks.Count).Where(j => RedirectHook(hooks[j])).ToList();
                if (ours.Count == hooks.Count && ours.Count > 0)
                    return (["hooks", "PreToolUse", i], "the csharp-indexer redirect hook (hooks.PreToolUse)");
                if (ours.Count > 0)
                    return (["hooks", "PreToolUse", i, "hooks", ours[0]], "the csharp-indexer redirect hook (hooks.PreToolUse)");
            }
        if ((root["permissions"] as JsonObject)?["allow"] is JsonArray allow && IndexOf(allow, "mcp__zazzle") is { } a)
            return (["permissions", "allow", a], "the mcp__zazzle permission (permissions.allow)");
        if (root["enabledMcpjsonServers"] is JsonArray enabled && IndexOf(enabled, "zazzle") is { } e)
            return (["enabledMcpjsonServers", e], "the zazzle entry of enabledMcpjsonServers");
        return null;
    }

    // True for the server init writes: stdio, a command ending in daemon, args ["mcp"], and nothing else.
    private static bool InitServer(JsonObject z) =>
        z.Count == 3 && Text(z["type"]) == "stdio" && Text(z["command"]) is { } c && (c == "daemon" || c.EndsWith("/daemon")) &&
        z["args"] is JsonArray { Count: 1 } args && Text(args[0]) == "mcp";

    // True for the hook init writes: a command ending in " hook" that names csharp-indexer.
    private static bool RedirectHook(JsonNode? hook) =>
        Text(hook?["command"]) is { } c && c.EndsWith(" hook") && c.Contains("csharp-indexer");

    // Returns the index of a string in a JSON array, or null.
    private static int? IndexOf(JsonArray list, string value)
    {
        for (var i = 0; i < list.Count; i++)
            if (Text(list[i]) == value)
                return i;
        return null;
    }

    // Returns the node at a path of property names and indexes, or null.
    private static JsonNode? At(JsonNode? node, object[] path)
    {
        foreach (var step in path)
            node = step is string name ? (node as JsonObject)?[name] : (node as JsonArray) is { } a && (int)step < a.Count ? a[(int)step] : null;
        return node;
    }

    // True when a node holds nothing but empty objects and arrays.
    private static bool Hollow(JsonNode? node) => node switch
    {
        JsonObject o => o.All(kv => Hollow(kv.Value)),
        JsonArray a => a.Count == 0,
        _ => false,
    };

    // Writes a path as permissions.allow or hooks.PreToolUse[0].
    private static string Dotted(object[] path) =>
        string.Concat(path.Select((p, i) => p is int n ? $"[{n}]" : (i == 0 ? "" : ".") + p));

    // Reads a JSON string value, or null.
    private static string? Text(JsonNode? n) => n is JsonValue v && v.TryGetValue<string>(out var s) ? s : null;

    // Parses a JSON object file, or null when it is missing or not a JSON object.
    private static JsonObject? Read(string file)
    {
        try
        {
            return File.Exists(file) ? JsonNode.Parse(File.ReadAllText(file)) as JsonObject : null;
        }
        catch (Exception e) when (e is JsonException or ArgumentException or InvalidOperationException)
        {
            return null;
        }
    }
}
