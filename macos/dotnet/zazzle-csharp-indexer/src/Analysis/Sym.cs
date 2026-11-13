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

    // Serialises identifier occurrences: name to line numbers, negative for declarations.
    public static string IdentsToJson(Dictionary<string, List<int>> idents)
    {
        var o = new JsonObject();
        foreach (var (k, v) in idents)
            o[k] = new JsonArray(v.Select(x => (JsonNode)x).ToArray());
        return o.ToJsonString();
    }

    // Parses stored identifier occurrences.
    public static Dictionary<string, List<int>> IdentsFromJson(string? json) =>
        json is null ? [] : JsonNode.Parse(json)!.AsObject().ToDictionary(kv => kv.Key, kv => kv.Value!.AsArray().Select(x => x!.GetValue<int>()).ToList());
}
