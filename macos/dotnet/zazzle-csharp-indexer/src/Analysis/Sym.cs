// A declared symbol in a file, stored as compact JSON per blob.
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Analysis;

// Name, kind, container (dotted), 1-based line span and the declaration's source line.
public sealed record Sym(string Name, string Kind, string Container, int Line, int EndLine, string Sig)
{
    // Serialises a symbol list as an array of arrays.
    public static string ToJson(IEnumerable<Sym> syms)
    {
        var arr = new JsonArray();
        foreach (var s in syms)
            arr.Add(new JsonArray(s.Name, s.Kind, s.Container, s.Line, s.EndLine, s.Sig));
        return arr.ToJsonString();
    }

    // Parses a stored symbol list.
    public static List<Sym> FromJson(string? json)
    {
        if (json is null)
            return [];
        return JsonNode.Parse(json)!.AsArray().Select(n =>
        {
            var a = n!.AsArray();
            return new Sym(a[0]!.GetValue<string>(), a[1]!.GetValue<string>(), a[2]!.GetValue<string>(), a[3]!.GetValue<int>(), a[4]!.GetValue<int>(), a[5]!.GetValue<string>());
        }).ToList();
    }

}
