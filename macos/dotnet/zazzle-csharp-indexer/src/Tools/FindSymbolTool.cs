// Tool that finds where a symbol is declared, replacing grep for definitions.
using System.Text.Json.Nodes;
using ZazzleIndexer.Analysis;

namespace ZazzleIndexer.Tools;

// Looks names up in the in-memory symbol name index, then reads details for matching blobs.
public sealed class FindSymbolTool(RootSet roots) : ITool
{
    public string Name => "find_symbol";

    // Finds matching declarations in every selected root; a dotted name (Namespace.Type.Member, or with ::) also needs its leading parts to end the declaration's container.
    public string Run(JsonNode? args, CallInfo info)
    {
        var query = Args.Str(args, "name").Replace("::", ".");
        var cut = query.LastIndexOf('.');
        var name = cut < 0 ? query : query[(cut + 1)..];
        var qualifier = cut < 0 ? "" : query[..cut];
        var exact = Args.Bool(args, "exact", false);
        var kind = Args.OptStr(args, "kind");
        var limit = Args.Int(args, "limit", 10);
        var offset = Args.Int(args, "offset", 0);
        info.Path = "index";
        var hits = new List<(int rank, string label, string text)>();
        foreach (var root in roots.Select(Args.OptStr(args, "root")))
        {
            if (!root.Index.WaitReady(Engine.ReadyTimeoutMs))
                throw new ToolException("index_cold", "index is still building; search with zazzle_find");
            var blobs = root.Index.Engine.BlobsDeclaring(name, exact);
            foreach (var f in root.Index.Files().Where(f => blobs.Contains(f.BlobId)))
                foreach (var s in Sym.FromJson(root.Index.Engine.Store.Symbols(f.BlobId)))
                    if ((exact ? s.Name.Equals(name, StringComparison.OrdinalIgnoreCase) : s.Name.Contains(name, StringComparison.OrdinalIgnoreCase)) && (kind is null || s.Kind == kind) && InContainer(s.Container, qualifier))
                        hits.Add((Rank(s, f.Path, name), $"{root.Name}:{f.Path}:{s.Line}{(s.EndLine > s.Line ? "-" + s.EndLine : "")}", $"{s.Kind} {ShortContainer(s.Container)}{s.Name} | {Clip(s.Sig)}"));
        }
        var ordered = hits.OrderBy(h => h.rank).ThenBy(h => h.label, StringComparer.Ordinal).ToList();
        var page = ordered.Skip(offset).Take(limit).Select(h => $"{h.label} {h.text}").ToList();
        info.Results = page.Count;
        info.Truncated = offset + page.Count < ordered.Count;
        return string.Join('\n', page.Append(info.Truncated ? $"[{page.Count} of {ordered.Count}; next offset {offset + page.Count}]" : $"[{ordered.Count} symbols]"));
    }

    private const int MaxSig = 80;
    private static readonly HashSet<string> TypeKinds = ["class", "struct", "interface", "enum", "record", "delegate", "type"];

    // Orders hits: exact name, then prefix, then word start, then substring; types before members; production code before tests.
    // A test symbol that only partly matches sorts after every production symbol, so test classes named after the query do not crowd out the code.
    private static int Rank(Sym s, string path, string name)
    {
        var match = s.Name.Equals(name, StringComparison.OrdinalIgnoreCase) ? 0
            : s.Name.StartsWith(name, StringComparison.OrdinalIgnoreCase) ? 1
            : WordStart(s.Name, name) ? 2 : 3;
        var test = IsTestPath(path);
        return (test && match > 0 ? 16 : 0) + match * 4 + (TypeKinds.Contains(s.Kind) ? 0 : 2) + (test ? 1 : 0);
    }

    // True when the name contains the query at a camelCase or underscore word boundary.
    private static bool WordStart(string name, string query)
    {
        for (var i = name.IndexOf(query, StringComparison.OrdinalIgnoreCase); i > 0; i = name.IndexOf(query, i + 1, StringComparison.OrdinalIgnoreCase))
            if (char.IsUpper(name[i]) || !char.IsLetterOrDigit(name[i - 1]))
                return true;
        return false;
    }

    // True for files in test projects or folders.
    private static bool IsTestPath(string path) =>
        path.Split('/').Any(seg => seg.Contains("Test", StringComparison.OrdinalIgnoreCase) || seg.Equals("tests", StringComparison.OrdinalIgnoreCase) || seg.Equals("Mocks", StringComparison.OrdinalIgnoreCase));

    // Keeps the type and member of a container, dropping the namespace.
    private static string ShortContainer(string container)
    {
        if (container == "")
            return "";
        var parts = container.Split('.');
        return string.Join('.', parts.TakeLast(2)) + ".";
    }

    // Cuts a signature to one short line.
    private static string Clip(string sig)
    {
        var one = System.Text.RegularExpressions.Regex.Replace(sig, @"\s+", " ").Trim();
        return one.Length <= MaxSig ? one : one[..(MaxSig - 1)].TrimEnd() + "…";
    }

    // True when the qualifier is empty or names the end of the container at a dot, ignoring case.
    private static bool InContainer(string container, string qualifier) =>
        qualifier == "" || container.Equals(qualifier, StringComparison.OrdinalIgnoreCase) || container.EndsWith("." + qualifier, StringComparison.OrdinalIgnoreCase);
}
