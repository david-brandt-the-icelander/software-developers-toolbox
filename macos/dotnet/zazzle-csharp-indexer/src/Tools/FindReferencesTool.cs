// Tool that lists identifier occurrences of a name, separating declarations from uses.
using System.Text.Json.Nodes;
using ZazzleIndexer.Analysis;

namespace ZazzleIndexer.Tools;

// Uses stored identifier tables: exact for C# (comments and strings excluded), word-based for other languages.
public sealed class FindReferencesTool(RootSet roots) : ITool
{
    public string Name => "find_references";
    public string Description => "Find where an identifier is used. Name-based, not type-resolved: same-named symbols are not distinguished. C# files exclude comments and strings [parsed]; other languages are word matches [heuristic]. D marks declarations.";
    public JsonNode Schema => Args.Schema(["name"],
        ("name", "string", "Exact identifier"),
        ("root", "string", "Limit to one root"),
        ("glob", "string", "Only files matching this glob"),
        ("limit", "integer", "Max reference lines (default 60)"),
        ("offset", "integer", "Skip this many lines (default 0)"),
        ("max_tokens", "integer", "Output budget (default 4000)"));

    // Selects candidate files by Bloom filter, then reads their identifier tables.
    public string Run(JsonNode? args, CallInfo info)
    {
        var name = Args.Str(args, "name");
        var glob = Args.OptStr(args, "glob") is { } g ? Glob.ToRegex(g) : null;
        var limit = Args.Int(args, "limit", 60);
        var skip = Args.Int(args, "offset", 0);
        var grams = Bloom.Grams([Bloom.Encode(name)]);
        var output = new Output(Args.Int(args, "max_tokens", 4000));
        info.Path = "index";
        int seen = 0, shown = 0;
        var stopped = false;
        foreach (var root in roots.Select(Args.OptStr(args, "root")))
        {
            if (!root.Index.WaitReady(Engine.ReadyTimeoutMs))
                throw new ToolException("index_cold", "index is still building; use search");
            var files = root.Index.Files().Where(f => glob is null || glob.IsMatch(f.Path)).ToList();
            root.Index.Barrier(files.Select(f => f.Path));
            foreach (var f in files)
            {
                var meta = root.Index.Engine.Blob(f.BlobId);
                if (meta is null || meta.Binary || (grams.Count > 0 && !Bloom.MayContainAll(meta.Bloom, grams)))
                    continue;
                var hits = Sym.IdentsFromJson(root.Index.Engine.Store.Idents(meta.Id)).GetValueOrDefault(name);
                if (hits is null)
                    continue;
                stopped = Render(root, f.Path, meta.Lang, hits, skip, limit, ref seen, ref shown, output);
                if (stopped)
                    break;
            }
            if (stopped)
                break;
        }
        info.Results = shown;
        info.Truncated = stopped;
        return output.Finish(stopped ? $"[{shown} shown; next offset {skip + shown}]" : $"[{shown} references]");
    }

    // Writes one file's reference lines; returns true when the page is full.
    private static bool Render(Root root, string rel, string lang, List<int> hits, int skip, int limit, ref int seen, ref int shown, Output output)
    {
        var text = Root.Decode(System.IO.File.ReadAllBytes(System.IO.Path.Combine(root.Path, rel))).Split('\n');
        var header = true;
        foreach (var h in hits.Distinct())
        {
            if (seen++ < skip)
                continue;
            if (shown >= limit || (header && !output.Add($"{root.Name}:{rel} [{(lang == "csharp" ? "parsed" : "heuristic")}]")))
                return true;
            header = false;
            var line = Math.Abs(h);
            if (!output.Add($"{(h < 0 ? "D" : "")}{line}: {(line <= text.Length ? text[line - 1].Trim() : "")}"))
                return true;
            shown++;
        }
        return false;
    }
}
