// Turns file bytes into the content-addressed facts stored for a blob.
using System.Security.Cryptography;
using System.Text;

namespace ZazzleIndexer.Analysis;

// Everything derived from one file's bytes.
public sealed record BlobData(string Oid, long Size, bool Binary, string Lang, int Lines, string Digest, string? Symbols);

// Static analyser; results depend only on the bytes and the file extension.
public static class BlobAnalyzer
{
    private const int MaxSymbolBytes = 2 * 1024 * 1024;

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

    // Analyses a file's bytes; binary files get only an id and size, oversized text skips symbols.
    public static BlobData Analyze(byte[] bytes, string path)
    {
        var oid = GitOid(bytes);
        var lang = Language(path);
        if (LooksBinary(bytes))
            return new BlobData(oid, bytes.Length, true, lang, 0, "binary", null);
        var lines = CountLines(bytes);
        if (bytes.Length > MaxSymbolBytes)
            return new BlobData(oid, bytes.Length, false, lang, lines, $"{lang}, {lines} lines (too large for symbols)", null);
        var text = Encoding.UTF8.GetString(bytes);
        var syms = lang == "csharp" ? CSharpSymbols.Extract(text) : HeuristicSymbols.Extract(text, lang);
        return new BlobData(oid, bytes.Length, false, lang, lines, Digest(lang, lines, syms), Sym.ToJson(syms));
    }

    // Summarises a file in one line: language, size, its type or heading names, and a member count.
    private static string Digest(string lang, int lines, List<Sym> syms)
    {
        var top = syms.Where(s => s.Kind is "class" or "struct" or "interface" or "record" or "enum" or "type" or "heading" && (s.Container == "" || lang == "csharp")).Select(s => s.Name).Distinct().Take(8).ToList();
        var members = syms.Count(s => s.Kind is not ("namespace" or "class" or "struct" or "interface" or "record" or "enum" or "type" or "heading" or "impl" or "mod"));
        var head = $"{lang}, {lines} lines";
        var parts = new List<string>();
        if (top.Count > 0)
            parts.Add(string.Join(", ", top));
        if (members > 0)
            parts.Add($"{members} members");
        return parts.Count == 0 ? head : $"{head}; {string.Join("; ", parts)}";
    }
}
