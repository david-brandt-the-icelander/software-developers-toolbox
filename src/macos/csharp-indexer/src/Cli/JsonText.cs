// Removes one property or array element from JSON text and keeps every other byte as it was, so a hand-edited file keeps its layout.
using System.Text.Json;

namespace CSharpIndexer.Cli;

// Static text-level JSON editing over UTF-8 bytes; a path is a list of property names (string) and array indexes (int).
public static class JsonText
{
    // Returns the JSON with the item at path cut out (with its separating comma), or null when nothing is there.
    public static byte[]? Remove(byte[] json, IReadOnlyList<object> path)
    {
        var span = path.Count == 0 ? null : Locate(json, path);
        if (span is null)
            return null;
        var (start, end) = span.Value;
        var before = Back(json, start);
        int from, to;
        if (json[before] == (byte)',')
            (from, to) = (before, end);
        else if (Forward(json, end) is var after && after < json.Length && json[after] == (byte)',')
            (from, to) = (start, Forward(json, after + 1));
        else
            (from, to) = (before + 1, Forward(json, end));
        return [.. json[..from], .. json[to..]];
    }

    // Finds the byte span of the item at path: from its property name (or value, in an array) to the end of its value.
    private static (int Start, int End)? Locate(byte[] json, IReadOnlyList<object> path)
    {
        var r = new Utf8JsonReader(json);
        return r.Read() ? Find(ref r, path, 0) : null;
    }

    // Looks for path[depth] inside the object or array the reader stands on.
    private static (int Start, int End)? Find(ref Utf8JsonReader r, IReadOnlyList<object> path, int depth)
    {
        if (r.TokenType == JsonTokenType.StartObject && path[depth] is string name)
        {
            while (r.Read() && r.TokenType == JsonTokenType.PropertyName)
            {
                var start = (int)r.TokenStartIndex;
                var match = r.ValueTextEquals(name);
                r.Read();
                if (match)
                    return Hit(ref r, path, depth, start);
                r.Skip();
            }
            return null;
        }
        if (r.TokenType == JsonTokenType.StartArray && path[depth] is int index)
            for (var i = 0; r.Read() && r.TokenType != JsonTokenType.EndArray; i++)
            {
                if (i == index)
                    return Hit(ref r, path, depth, (int)r.TokenStartIndex);
                r.Skip();
            }
        return null;
    }

    // Descends further when the path goes on, or returns the span of the value the reader stands on.
    private static (int Start, int End)? Hit(ref Utf8JsonReader r, IReadOnlyList<object> path, int depth, int start)
    {
        if (depth < path.Count - 1)
            return Find(ref r, path, depth + 1);
        r.Skip();
        return (start, (int)r.BytesConsumed);
    }

    // Returns the index of the last non-whitespace byte before i.
    private static int Back(byte[] json, int i)
    {
        do
            i--;
        while (i > 0 && Space(json[i]));
        return i;
    }

    // Returns the index of the first non-whitespace byte at or after i.
    private static int Forward(byte[] json, int i)
    {
        while (i < json.Length && Space(json[i]))
            i++;
        return i;
    }

    // True for JSON whitespace.
    private static bool Space(byte b) => b is (byte)' ' or (byte)'\t' or (byte)'\n' or (byte)'\r';
}
