// Tool that returns a terse behavioural view of a C# file, or of C# text a caller passes (the code blocks of a Razor file), with source line numbers and no hash.
using System.Text.Json.Nodes;
using ZazzleIndexer.View;

namespace ZazzleIndexer.Tools;

// Shows what a C# file does without usings, logging, trivial constructors, log-and-rethrow catches or access modifiers.
public sealed class ViewTool(RootSet roots) : ITool
{
    public string Name => "view";

    // Builds the view of the file, or of `text` when given (the path then only names it): a header line, then the view lines.
    public string Run(JsonNode? args, CallInfo info)
    {
        var (root, rel) = roots.Split(Args.Str(args, "path"));
        var full = root.Resolve(rel) ?? throw new ToolException("outside_root", $"path is outside the root: {rel}");
        var text = Args.OptStr(args, "text");
        if (text is null && !rel.EndsWith(".cs", StringComparison.OrdinalIgnoreCase))
            throw new ToolException("not_csharp", $"not a C# file: {root.Name}:{rel}");
        if (text is null && !File.Exists(full))
            throw new ToolException("not_found", $"no such file: {root.Name}:{rel}");
        info.Path = "view";
        var source = text ?? Root.Decode(File.ReadAllBytes(full));
        var (lines, steps) = TerseView.Build(source);
        info.Results = lines.Count;
        var total = source.Split('\n').Length - (source.EndsWith('\n') ? 1 : 0);
        var summary = steps.Count == 0 ? "nothing to shorten" : string.Join(", ", steps.Select(s => $"{s.Count} {s.Step}"));
        return $"{root.Name}:{rel} {total} lines; terse: {summary}\n{TerseView.Format(lines)}";
    }
}
