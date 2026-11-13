// Tool that lists files by name substring and glob, replacing find and ls.
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Tools;

// Lists non-ignored files of the roots.
public sealed class FindFilesTool(RootSet roots) : ITool
{
    public string Name => "find_files";
    public string Description => "List files (git-tracked or untracked, not ignored) by case-insensitive name substring and/or glob (** crosses directories). Replaces find and ls. Output: root:path per line.";
    public JsonNode Schema => Args.Schema([],
        ("query", "string", "Case-insensitive substring of the path"),
        ("glob", "string", "Glob such as src/**/*.cs; without / it matches file names at any depth"),
        ("root", "string", "Limit to one root"),
        ("limit", "integer", "Max files (default 100)"),
        ("offset", "integer", "Skip this many matches (default 0)"));

    // Filters the file lists and pages the result.
    public string Run(JsonNode? args, CallInfo info)
    {
        var query = Args.OptStr(args, "query");
        var glob = Args.OptStr(args, "glob") is { } g ? Glob.ToRegex(g) : null;
        var limit = Args.Int(args, "limit", 100);
        var offset = Args.Int(args, "offset", 0);
        info.Path = "index";
        var matches = new List<string>();
        foreach (var root in roots.Select(Args.OptStr(args, "root")))
            foreach (var f in root.Listing(info))
                if ((query is null || f.Contains(query, StringComparison.OrdinalIgnoreCase)) && (glob is null || glob.IsMatch(f)))
                    matches.Add($"{root.Name}:{f}");
        var page = matches.Skip(offset).Take(limit).ToList();
        info.Results = page.Count;
        var next = offset + page.Count;
        info.Truncated = next < matches.Count;
        var footer = info.Truncated ? $"[{page.Count} of {matches.Count}; next offset {next}]" : $"[{matches.Count} files]";
        return string.Join('\n', page.Append(footer));
    }
}
