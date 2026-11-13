// Tool that reads a line range of a file, replacing cat, head, tail and sed -n.
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Tools;

// Reads text files with line numbers under a token budget, skipping content the agent already has.
public sealed class ReadTool(RootSet roots) : ITool
{
    private readonly Dictionary<string, string> _seen = [];

    public string Name => "read";
    public string Description => "Read a text file by line range with line numbers. Replaces cat, head, tail and sed -n. Path is root:relative/path. A repeat read of an unchanged range returns a short 'unchanged' note instead of the text.";
    public JsonNode Schema => Args.Schema(["path"],
        ("path", "string", "root:relative/path"),
        ("start_line", "integer", "First line, 1-based (default 1)"),
        ("end_line", "integer", "Last line, inclusive (default: as many as fit)"),
        ("max_tokens", "integer", "Output budget (default 4000)"));

    // Reads the range and stops at the budget with a continuation hint.
    public string Run(JsonNode? args, CallInfo info)
    {
        var (root, rel) = roots.Split(Args.Str(args, "path"));
        var full = root.Resolve(rel) ?? throw new ToolException("outside_root", $"path is outside the root: {rel}");
        if (!File.Exists(full))
            throw new ToolException("not_found", $"no such file: {root.Name}:{rel}");
        var bytes = File.ReadAllBytes(full);
        if (Analysis.BlobAnalyzer.LooksBinary(bytes))
            throw new ToolException("binary", $"binary file: {root.Name}:{rel}");
        info.Path = "index";
        var start = Math.Max(1, Args.Int(args, "start_line", 1));
        var end = Args.Int(args, "end_line", int.MaxValue);
        var oid = Analysis.BlobAnalyzer.GitOid(bytes);
        var key = $"{root.Name}:{rel}:{start}-{end}";
        if (_seen.TryGetValue(key, out var prev) && prev == oid)
        {
            info.Reason = "unchanged_since_last_read";
            return $"{root.Name}:{rel} lines {start}-{(end == int.MaxValue ? "end" : end)} unchanged since your last read (blob {oid[..8]})";
        }
        var lines = Root.Decode(bytes).Split('\n');
        if (lines.Length > 0 && lines[^1] == "")
            lines = lines[..^1];
        var output = new Output(Args.Int(args, "max_tokens", 4000));
        var last = start - 1;
        for (var n = start; n <= Math.Min(end, lines.Length); n++)
        {
            if (!output.Add($"{n}\t{lines[n - 1].TrimEnd('\r')}"))
                break;
            last = n;
        }
        info.Results = last - start + 1;
        info.Truncated = last < Math.Min(end, lines.Length);
        if (!info.Truncated)
            _seen[key] = oid;
        var head = $"{root.Name}:{rel} lines {start}-{last} of {lines.Length}";
        return info.Truncated ? output.Finish($"[truncated; continue with start_line {last + 1}]") + "\n" + head : output.Finish(head);
    }
}
