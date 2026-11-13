// Tool that finds where a symbol is declared, replacing grep for definitions.
using System.Text.Json.Nodes;
using ZazzleIndexer.Analysis;

namespace ZazzleIndexer.Tools;

// Looks names up in the in-memory symbol name index, then reads details for matching blobs.
public sealed class FindSymbolTool(RootSet roots) : ITool
{
    public string Name => "find_symbol";

    // Finds matching declarations in every selected root.
    public string Run(JsonNode? args, CallInfo info)
    {
        var name = Args.Str(args, "name");
        var exact = Args.Bool(args, "exact", false);
        var kind = Args.OptStr(args, "kind");
        var limit = Args.Int(args, "limit", 30);
        var offset = Args.Int(args, "offset", 0);
        info.Path = "index";
        var hits = new List<(bool exact, string label, string text)>();
        foreach (var root in roots.Select(Args.OptStr(args, "root")))
        {
            if (!root.Index.WaitReady(Engine.ReadyTimeoutMs))
                throw new ToolException("index_cold", "index is still building; search with zazzle_find");
            var blobs = root.Index.Engine.BlobsDeclaring(name, exact);
            foreach (var f in root.Index.Files().Where(f => blobs.Contains(f.BlobId)))
                foreach (var s in Sym.FromJson(root.Index.Engine.Store.Symbols(f.BlobId)))
                    if ((exact ? s.Name.Equals(name, StringComparison.OrdinalIgnoreCase) : s.Name.Contains(name, StringComparison.OrdinalIgnoreCase)) && (kind is null || s.Kind == kind))
                        hits.Add((s.Name.Equals(name, StringComparison.OrdinalIgnoreCase), $"{root.Name}:{f.Path}:{s.Line}", $"{s.Kind} {(s.Container == "" ? "" : s.Container + ".")}{s.Name} | {s.Sig}"));
        }
        var ordered = hits.OrderByDescending(h => h.exact).ThenBy(h => h.label, StringComparer.Ordinal).ToList();
        var page = ordered.Skip(offset).Take(limit).Select(h => $"{h.label} {h.text}").ToList();
        info.Results = page.Count;
        info.Truncated = offset + page.Count < ordered.Count;
        return string.Join('\n', page.Append(info.Truncated ? $"[{page.Count} of {ordered.Count}; next offset {offset + page.Count}]" : $"[{ordered.Count} symbols]"));
    }
}
