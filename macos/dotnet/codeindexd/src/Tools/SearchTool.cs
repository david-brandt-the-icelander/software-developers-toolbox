// Tool that searches file contents with a regex, replacing grep and rg.
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace CodeIndexd.Tools;

// Scans candidate files in parallel and reports matches grouped by file.
public sealed class SearchTool(RootSet roots) : ITool
{
    private const long MaxFileBytes = 2 * 1024 * 1024;
    private const string Cheatsheet = "Dialect is .NET regex: lookarounds, backreferences, \\d \\b \\s, (?i) and (?s) work; possessive quantifiers, recursion and \\K do not. Use literal=true for plain text.";

    public string Name => "search";
    public string Description => "Search file contents (.NET regex, matched per line). Replaces grep and rg. Output: root:path then 'line:text' lines, with optional context.";
    public JsonNode Schema => Args.Schema(["pattern"],
        ("pattern", "string", "Regex, or plain text with literal=true"),
        ("literal", "boolean", "Treat pattern as plain text"),
        ("ignore_case", "boolean", "Case-insensitive"),
        ("glob", "string", "Only files matching this glob"),
        ("root", "string", "Limit to one root"),
        ("context", "integer", "Context lines around matches (default 0)"),
        ("max_results", "integer", "Max matching lines (default 50)"),
        ("offset", "integer", "Skip this many matches (default 0)"),
        ("max_tokens", "integer", "Output budget (default 4000)"));

    // Compiles the pattern, scans files in path order and renders the page.
    public string Run(JsonNode? args, CallInfo info)
    {
        var pattern = Args.Str(args, "pattern");
        var regex = Compile(pattern, Args.Bool(args, "literal", false), Args.Bool(args, "ignore_case", false));
        var glob = Args.OptStr(args, "glob") is { } g ? Glob.ToRegex(g) : null;
        var context = Math.Max(0, Args.Int(args, "context", 0));
        var limit = Args.Int(args, "max_results", 50);
        var skip = Args.Int(args, "offset", 0);
        info.Path = "scan";
        info.Reason = "index_not_built";
        var files = roots.Select(Args.OptStr(args, "root"))
            .SelectMany(r => r.ListFiles().Where(f => glob is null || glob.IsMatch(f)).Select(f => (root: r, rel: f)))
            .ToList();
        var output = new Output(Args.Int(args, "max_tokens", 4000));
        var seen = 0;
        var shown = 0;
        var stoppedEarly = false;
        foreach (var file in files.AsParallel().AsOrdered().Select(f => ScanFile(f.root, f.rel, regex)))
        {
            if (file is null)
                continue;
            var header = true;
            var lastShown = -1;
            foreach (var m in file.Matches)
            {
                if (seen++ < skip)
                    continue;
                if (shown >= limit)
                {
                    stoppedEarly = true;
                    break;
                }
                if (header)
                {
                    stoppedEarly = !output.Add(file.Label);
                    header = false;
                }
                for (var i = Math.Max(Math.Max(0, m - context), lastShown + 1); i <= Math.Min(file.Text.Length - 1, m + context) && !stoppedEarly; i++)
                {
                    stoppedEarly = !output.Add($"{i + 1}{(i == m ? ':' : '-')}{file.Text[i]}");
                    lastShown = i;
                }
                if (stoppedEarly)
                    break;
                shown++;
            }
            if (stoppedEarly)
                break;
        }
        info.Results = shown;
        info.Truncated = stoppedEarly;
        return output.Finish(stoppedEarly ? $"[{shown} shown; next offset {skip + shown}]" : $"[{shown} matches]");
    }

    // Builds the regex, turning compile errors into a tool error with the dialect notes.
    private static Regex Compile(string pattern, bool literal, bool ignoreCase)
    {
        var options = RegexOptions.CultureInvariant | (ignoreCase ? RegexOptions.IgnoreCase : RegexOptions.None);
        try
        {
            return new Regex(literal ? Regex.Escape(pattern) : pattern, options, TimeSpan.FromSeconds(2));
        }
        catch (ArgumentException e)
        {
            throw new ToolException("bad_regex", $"{e.Message}\n{Cheatsheet}");
        }
    }

    // Returns the matching line indexes of one file, or null when it has no match or is skipped.
    private static FileHits? ScanFile(Root root, string rel, Regex regex)
    {
        var full = root.Resolve(rel);
        if (full is null || new FileInfo(full).Length > MaxFileBytes || Root.IsBinary(full))
            return null;
        var text = Root.ReadText(full).Split('\n').Select(l => l.TrimEnd('\r')).ToArray();
        var matched = new List<int>();
        for (var i = 0; i < text.Length; i++)
        {
            try
            {
                if (regex.IsMatch(text[i]))
                    matched.Add(i);
            }
            catch (RegexMatchTimeoutException)
            {
                break;
            }
        }
        return matched.Count == 0 ? null : new FileHits($"{root.Name}:{rel}", text, matched);
    }

    // The lines of one file and the indexes of those that matched.
    private sealed record FileHits(string Label, string[] Text, List<int> Matches);
}
