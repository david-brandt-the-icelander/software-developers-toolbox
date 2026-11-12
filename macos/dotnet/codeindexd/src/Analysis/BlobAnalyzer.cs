// Turns file bytes into the content-addressed facts stored for a blob.
using System.Security.Cryptography;
using System.Text;

namespace CodeIndexd.Analysis;

// Everything derived from one file's bytes.
public sealed record BlobData(string Oid, long Size, bool Binary, string Lang, int Lines, byte[] Bloom, string Digest, string? Symbols, string? Idents);

// Static analyser; results depend only on the bytes and the file extension.
public static class BlobAnalyzer
{
    private static readonly Dictionary<string, string> Langs = new(StringComparer.OrdinalIgnoreCase)
    {
        [".cs"] = "csharp", [".rs"] = "rust", [".ts"] = "typescript", [".tsx"] = "typescript", [".js"] = "javascript",
        [".py"] = "python", [".go"] = "go", [".java"] = "java", [".md"] = "markdown", [".json"] = "json",
        [".yml"] = "yaml", [".yaml"] = "yaml", [".toml"] = "toml", [".sh"] = "shell", [".csproj"] = "xml", [".xml"] = "xml",
        [".c"] = "c", [".h"] = "c", [".cpp"] = "cpp", [".swift"] = "swift", [".gd"] = "gdscript", [".sql"] = "sql",
    };

    // Computes git's blob object id: SHA-1 of "blob <length>\0" followed by the bytes.
    public static string GitOid(byte[] bytes)
    {
        using var sha = IncrementalHash.CreateHash(HashAlgorithmName.SHA1);
        sha.AppendData(Encoding.ASCII.GetBytes($"blob {bytes.Length}\0"));
        sha.AppendData(bytes);
        return Convert.ToHexString(sha.GetHashAndReset()).ToLowerInvariant();
    }

    // Names the language from the file extension, or "text".
    public static string Language(string path) => Langs.GetValueOrDefault(System.IO.Path.GetExtension(path), "text");

    // Reports whether the first 8 KiB contains a NUL byte.
    public static bool LooksBinary(byte[] bytes) => Array.IndexOf(bytes, (byte)0, 0, Math.Min(bytes.Length, 8192)) >= 0;

    // Counts lines: newline characters plus one for an unterminated last line.
    public static int CountLines(byte[] bytes)
    {
        var n = 0;
        foreach (var b in bytes)
            if (b == (byte)'\n')
                n++;
        return bytes.Length > 0 && bytes[^1] != (byte)'\n' ? n + 1 : n;
    }

    // Analyses a file's bytes; binary files get only an id and size.
    public static BlobData Analyze(byte[] bytes, string path)
    {
        var oid = GitOid(bytes);
        var lang = Language(path);
        if (LooksBinary(bytes))
            return new BlobData(oid, bytes.Length, true, lang, 0, [], "binary", null, null);
        var lines = CountLines(bytes);
        return new BlobData(oid, bytes.Length, false, lang, lines, Bloom.Build(bytes), $"{lang}, {lines} lines", null, null);
    }
}
