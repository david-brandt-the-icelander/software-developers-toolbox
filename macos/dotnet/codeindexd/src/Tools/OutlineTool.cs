// Tool that lists the symbols of a file, or one-line digests of the files in a directory.
using System.Text.Json.Nodes;
using CodeIndexd.Analysis;

namespace CodeIndexd.Tools;

// Gives structure without reading the file body.
public sealed class OutlineTool(RootSet roots) : ITool
{
    public string Name => "outline";
    public string Description => "Structure of code without reading it. For a file: its declarations with line ranges and signatures. For a directory (path ending in /, or root:): a one-line digest per file. Use before read.";
    public JsonNode Schema => Args.Schema(["path"],
        ("path", "string", "root:file, root:dir/ or root:"),
        ("limit", "integer", "Max files for a directory (default 100)"),
        ("offset", "integer", "Skip this many files (default 0)"));

    // Dispatches on file versus directory.
    public string Run(JsonNode? args, CallInfo info)
    {
        var raw = Args.Str(args, "path");
        var (root, rel) = roots.Split(raw.EndsWith(':') ? raw + "/" : raw);
        info.Path = "index";
        if (rel == "" || rel.EndsWith('/') || System.IO.Directory.Exists(root.Resolve(rel) ?? ""))
            return OutlineDir(root, rel.TrimEnd('/'), Args.Int(args, "limit", 100), Args.Int(args, "offset", 0), info);
        return OutlineFile(root, rel, info);
    }

    // Lists one file's declarations indented by nesting.
    private static string OutlineFile(Root root, string rel, CallInfo info)
    {
        var entry = root.Index.Freshen(rel) ?? throw new ToolException("not_found", $"no such file: {root.Name}:{rel}");
        var meta = entry.BlobId == 0 ? null : root.Index.Engine.Blob(entry.BlobId);
        if (meta is null || meta.Binary)
            throw new ToolException("no_outline", $"no outline for {root.Name}:{rel} (binary or too large)");
        var syms = Sym.FromJson(root.Index.Engine.Store.Symbols(meta.Id));
        info.Results = syms.Count;
        var tag = meta.Lang == "csharp" ? "parsed" : "heuristic";
        var lines = syms.Select(s => $"{new string(' ', s.Container == "" ? 0 : 2 * (s.Container.Count(c => c == '.') + 1))}{s.Line}-{s.EndLine} {s.Kind} {s.Name}  {s.Sig}");
        return string.Join('\n', lines.Prepend($"{root.Name}:{rel} [{tag}; {meta.Digest}]"));
    }

    // Lists a digest per file under a directory prefix.
    private static string OutlineDir(Root root, string prefix, int limit, int offset, CallInfo info)
    {
        var files = root.Index.Files().Where(f => prefix == "" || f.Path.StartsWith(prefix + "/", StringComparison.Ordinal)).ToList();
        var page = files.Skip(offset).Take(limit).Select(f => $"{f.Path}: {(f.BlobId == 0 ? "large file" : root.Index.Engine.Blob(f.BlobId)?.Digest)}").ToList();
        info.Results = page.Count;
        info.Truncated = offset + page.Count < files.Count;
        return string.Join('\n', page.Append(info.Truncated ? $"[{page.Count} of {files.Count}; next offset {offset + page.Count}]" : $"[{files.Count} files]"));
    }
}
