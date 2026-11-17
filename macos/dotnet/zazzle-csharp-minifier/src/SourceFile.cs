// A C# file read as UTF-8, remembering its byte-order mark so rewrites keep it.
using System.Text;

namespace ZazzleMinifier;

// The text of one source file, its path relative to the root with / separators, and whether it had a BOM.
internal sealed record SourceFile(string Path, string Rel, string Text, bool Bom)
{
    // Reads the file, or returns null with the reason when it is not valid UTF-8.
    public static SourceFile? Read(string root, string path, out string? error)
    {
        error = null;
        var bytes = File.ReadAllBytes(path);
        var bom = bytes.AsSpan().StartsWith(Encoding.UTF8.Preamble);
        var rel = System.IO.Path.GetRelativePath(root, path).Replace('\\', '/');
        try
        {
            var text = new UTF8Encoding(false, true).GetString(bytes, bom ? 3 : 0, bytes.Length - (bom ? 3 : 0));
            return new SourceFile(path, rel, text, bom);
        }
        catch (DecoderFallbackException)
        {
            error = "not UTF-8";
            return null;
        }
    }

    // True for D/src/N.cs when D/N.cs exists: the implementation half that --split-headers wrote, which the other commands leave alone.
    public static bool IsImplementationHalf(string path)
    {
        var dir = System.IO.Path.GetDirectoryName(path)!;
        if (System.IO.Path.GetFileName(dir) != "src")
            return false;
        return File.Exists(System.IO.Path.Combine(System.IO.Path.GetDirectoryName(dir)!, System.IO.Path.GetFileName(path)));
    }
}
