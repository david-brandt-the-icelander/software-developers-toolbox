// Tool that reads a line range of a file, replacing cat, head, tail and sed -n.
using System.Text.Json.Nodes;

namespace CodeIndexd.Tools;

// Reads text files with line numbers under a token budget.
public sealed class ReadTool(RootSet roots) : ITool
{
    public string Name => "read";
    public string Description => "Read a text file by line range with line numbers. Replaces cat, head, tail and sed -n. Path is root:relative/path.";
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
        if (Root.IsBinary(full))
            throw new ToolException("binary", $"binary file: {root.Name}:{rel}");
        info.Path = "scan";
        info.Reason = "index_not_built";
        var start = Math.Max(1, Args.Int(args, "start_line", 1));
        var end = Args.Int(args, "end_line", int.MaxValue);
        var lines = Root.ReadText(full).Split('\n');
        if (lines.Length > 0 && lines[^1] == "")
            lines = lines[..^1];
        var output = new Output(Args.Int(args, "max_tokens", 4000));
        var last = start - 1;
        for (var n = start; n <= Math.Min(end, lines.Length); n++)
        {
            if (!output.Add($"{n}\t{lines[n - 1]}"))
                break;
            last = n;
        }
        info.Results = last - start + 1;
        info.Truncated = last < Math.Min(end, lines.Length);
        var head = $"{root.Name}:{rel} lines {start}-{last} of {lines.Length}";
        return info.Truncated ? output.Finish($"[truncated; continue with start_line {last + 1}]") + "\n" + head : output.Finish(head);
    }
}
