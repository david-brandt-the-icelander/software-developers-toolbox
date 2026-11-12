// Tool that searches file contents with a regex, replacing grep and rg.
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;
using CodeIndexd.Analysis;

namespace CodeIndexd.Tools;

// Prefilters files with the trigram index, verifies matches on disk, and reports them grouped by file.
public sealed class SearchTool(RootSet roots) : ITool
{
    private const long MaxFileBytes = 2 * 1024 * 1024;
    private const string Cheatsheet = "Dialect is .NET regex: lookarounds, backreferences, \\d \\b \\s, (?i) and (?s) work; possessive quantifiers, recursion and \\K do not. Use literal=true for plain text.";

    public string Name => "search";
    public string Description => "Search file contents (.NET regex, matched per line), index-accelerated. Replaces grep and rg. mode=lines (default) prints 'line:text' under each file; files lists matching files; count gives matches per file.";
    public JsonNode Schema => Args.Schema(["pattern"],
        ("pattern", "string", "Regex, or plain text with literal=true"),
        ("literal", "boolean", "Treat pattern as plain text"),
        ("ignore_case", "boolean", "Case-insensitive"),
        ("glob", "string", "Only files matching this glob"),
        ("root", "string", "Limit to one root"),
        ("mode", "string", "lines (default), files or count"),
        ("context", "integer", "Context lines around matches (default 0, lines mode)"),
        ("max_results", "integer", "Max matching lines or files (default 50)"),
        ("offset", "integer", "Skip this many results (default 0)"),
        ("max_tokens", "integer", "Output budget (default 4000)"));

    // Compiles the pattern, selects candidate files, scans them in path order and renders the page.
    public string Run(JsonNode? args, CallInfo info)
    {
        var pattern = Args.Str(args, "pattern");
        var literal = Args.Bool(args, "literal", false);
        var regex = Compile(pattern, literal, Args.Bool(args, "ignore_case", false));
        var glob = Args.OptStr(args, "glob") is { } g ? Glob.ToRegex(g) : null;
        var mode = Args.OptStr(args, "mode") ?? "lines";
        var context = Math.Max(0, Args.Int(args, "context", 0));
        var limit = Args.Int(args, "max_results", 50);
        var skip = Args.Int(args, "offset", 0);
        var grams = Bloom.Grams(RegexLiterals.Required(pattern, literal).Select(Bloom.Encode));
        var candidates = Candidates(args, glob, grams, info);
        var output = new Output(Args.Int(args, "max_tokens", 4000));
        var seen = 0;
        var shown = 0;
        var stopped = false;
        foreach (var file in candidates.AsParallel().AsOrdered().Select(c => ScanFile(c.root, c.rel, regex)))
        {
            if (file is null)
                continue;
            if (mode != "lines")
            {
                if (seen++ < skip)
                    continue;
                if (shown >= limit || !output.Add(mode == "count" ? $"{file.Label}:{file.Matches.Count}" : file.Label))
                {
                    stopped = true;
                    break;
                }
                shown++;
                continue;
            }
            stopped = RenderLines(file, context, skip, limit, ref seen, ref shown, output);
            if (stopped)
                break;
        }
        info.Results = shown;
        info.Truncated = stopped;
        var unit = mode == "lines" ? "matches" : "files";
        return output.Finish(stopped ? $"[{shown} shown; next offset {skip + shown}]" : $"[{shown} {unit}]");
    }

    // Writes one file's matching lines (with context) into the output; returns true when the page is full.
    private static bool RenderLines(FileHits file, int context, int skip, int limit, ref int seen, ref int shown, Output output)
    {
        var header = true;
        var lastShown = -1;
        foreach (var m in file.Matches)
        {
            if (seen++ < skip)
                continue;
            if (shown >= limit)
                return true;
            if (header && !output.Add(file.Label))
                return true;
            header = false;
            for (var i = Math.Max(Math.Max(0, m - context), lastShown + 1); i <= Math.Min(file.Text.Length - 1, m + context); i++)
            {
                if (!output.Add($"{i + 1}{(i == m ? ':' : '-')}{file.Text[i]}"))
                    return true;
                lastShown = i;
            }
            shown++;
        }
        return false;
    }

    // Picks files to scan: index-filtered when every root is ready, otherwise a plain scan of the git listing.
    private List<(Root root, string rel)> Candidates(JsonNode? args, Regex? glob, List<int> grams, CallInfo info)
    {
        var result = new List<(Root, string)>();
        var total = 0;
        foreach (var root in roots.Select(Args.OptStr(args, "root")))
        {
            if (!root.Index.WaitReady(Engine.ReadyTimeoutMs))
            {
                info.Path = "scan";
                info.Reason = "index_cold";
                foreach (var f in root.Listing(info).Where(f => glob is null || glob.IsMatch(f)))
                    result.Add((root, f));
                continue;
            }
            var entries = root.Index.Files().Where(f => glob is null || glob.IsMatch(f.Path)).ToList();
            root.Index.Barrier(entries.Select(e => e.Path));
            total += entries.Count;
            foreach (var e in root.Index.Files().Where(f => glob is null || glob.IsMatch(f.Path)))
                if (MayMatch(root, e.BlobId, grams))
                    result.Add((root, e.Path));
        }
        info.TotalFiles = total;
        info.Candidates = result.Count;
        return result;
    }

    // Applies the Bloom prefilter; unanalysed (too large) files always stay candidates.
    private static bool MayMatch(Root root, int blobId, List<int> grams)
    {
        if (blobId == 0)
            return true;
        var meta = root.Index.Engine.Blob(blobId);
        if (meta is null || meta.Binary)
            return false;
        return grams.Count == 0 || Bloom.MayContainAll(meta.Bloom, grams);
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
        if (full is null || !File.Exists(full) || new FileInfo(full).Length > MaxFileBytes)
            return null;
        var bytes = File.ReadAllBytes(full);
        if (BlobAnalyzer.LooksBinary(bytes))
            return null;
        var text = Root.Decode(bytes).Split('\n').Select(l => l.TrimEnd('\r')).ToArray();
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
