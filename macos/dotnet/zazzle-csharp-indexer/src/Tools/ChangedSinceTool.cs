// Tool that lists files changed since an earlier cursor, so an agent can refresh its picture cheaply.
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Tools;

// Cursors are "epoch;root=gen;root=gen" and only valid within one daemon run.
public sealed class ChangedSinceTool(RootSet roots) : ITool
{
    public string Name => "changed_since";
    public string Description => "List files added (A), modified (M) or deleted (D) since a cursor from an earlier call. Call without a cursor to get a starting cursor. Cursors expire when the daemon restarts.";
    public JsonNode Schema => Args.Schema([], ("cursor", "string", "Cursor from a previous call"));

    // Returns the changes since the cursor and a new cursor.
    public string Run(JsonNode? args, CallInfo info)
    {
        info.Path = "index";
        var cursor = Args.OptStr(args, "cursor");
        var epoch = roots.All[0].Index.Engine.Epoch;
        var next = epoch + ";" + string.Join(';', roots.All.Select(r => $"{r.Name}={r.Index.Gen}"));
        if (cursor is null)
            return $"cursor: {next}";
        var parts = cursor.Split(';');
        if (parts[0] != epoch)
            return $"cursor expired (daemon restarted); re-list with find_files. new cursor: {next}";
        var gens = parts.Skip(1).Select(p => p.Split('=')).Where(p => p.Length == 2).ToDictionary(p => p[0], p => long.Parse(p[1]));
        var lines = new List<string>();
        foreach (var root in roots.All.Where(r => gens.ContainsKey(r.Name)))
        {
            root.Index.Sync();
            var changes = root.Index.ChangesSince(gens[root.Name], out var expired);
            if (expired)
                lines.Add($"{root.Name}: change log too old; re-list with find_files");
            lines.AddRange(changes.OrderBy(c => c.path, StringComparer.Ordinal).Select(c => $"{c.kind} {root.Name}:{c.path}"));
        }
        info.Results = lines.Count;
        next = epoch + ";" + string.Join(';', roots.All.Select(r => $"{r.Name}={r.Index.Gen}"));
        return string.Join('\n', lines.Append($"cursor: {next}"));
    }
}
