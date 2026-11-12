// Tool that finds where a symbol is declared, replacing grep for definitions.
using System.Text.Json.Nodes;
using CodeIndexd.Analysis;

namespace CodeIndexd.Tools;

// Looks names up in the in-memory symbol name index, then reads details for matching blobs.
public sealed class FindSymbolTool(RootSet roots) : ITool
{
    public string Name => "find_symbol";
    public string Description => "Find declarations by name (case-insensitive; substring unless exact=true). Output: root:path:line kind container.name | signature. Exact matches first.";
    public JsonNode Schema => Args.Schema(["name"],
        ("name", "string", "Symbol name or part of it"),
        ("exact", "boolean", "Whole-name match only"),
        ("kind", "string", "Filter by kind: class, method, property, field, fn, def ..."),
        ("root", "string", "Limit to one root"),
        ("limit", "integer", "Max results (default 30)"),
        ("offset", "integer", "Skip this many results (default 0)"));

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
                throw new ToolException("index_cold", "index is still building; use search");
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
