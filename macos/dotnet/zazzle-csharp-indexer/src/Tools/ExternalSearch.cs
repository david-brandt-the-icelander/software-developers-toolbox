// Fallback for regexes the built-in engine rejects: runs rg or git grep with PCRE2 and reshapes their output.
namespace ZazzleIndexer.Tools;

// Static helper; returns null when no external engine can run the pattern.
public static class ExternalSearch
{
    private static readonly Lazy<bool> HasRg = new(() => Proc.Run("rg", ["--version"], 5).ExitCode == 0);

    // Searches the roots with an external engine and formats results like the built-in search.
    public static string? Try(IEnumerable<Root> roots, string pattern, bool ignoreCase, string? glob, int limit, int skip, int maxTokens, CallInfo info)
    {
        var output = new Output(maxTokens);
        int seen = 0, shown = 0;
        var stopped = false;
        var tool = "";
        foreach (var root in roots)
        {
            var r = HasRg.Value ? RunRg(root, pattern, ignoreCase, glob) : RunGitGrep(root, pattern, ignoreCase, glob);
            tool = HasRg.Value ? "rg" : "git-grep";
            if (r.ExitCode > 1)
                return null;
            string? lastFile = null;
            foreach (var hit in r.Stdout.Split('\n', StringSplitOptions.RemoveEmptyEntries).Select(Parse).Where(h => h is not null).OrderBy(h => h!.Value.file, StringComparer.Ordinal).ThenBy(h => h!.Value.line))
            {
                if (seen++ < skip)
                    continue;
                var (file, line, text) = hit!.Value;
                if (shown >= limit || (lastFile != file && !output.Add($"{root.Name}:{file}")) || !output.Add($"{line}:{text}"))
                {
                    stopped = true;
                    break;
                }
                lastFile = file;
                shown++;
            }
            if (stopped)
                break;
        }
        info.Path = "fallback:" + tool;
        info.Reason = "regex_unsupported";
        info.Results = shown;
        info.Truncated = stopped;
        var note = $"fallback {tool} -P; context and mode not applied";
        return output.Finish(stopped ? $"[{shown} shown; next offset {skip + shown}; {note}]" : $"[{shown} matches; {note}]");
    }

    // Runs ripgrep with PCRE2.
    private static ProcResult RunRg(Root root, string pattern, bool ignoreCase, string? glob)
    {
        var a = new List<string> { "--no-heading", "-n", "--color", "never", "-P", "--max-columns", "300" };
        if (ignoreCase)
            a.Add("-i");
        if (glob is not null)
            a.AddRange(["-g", glob]);
        a.AddRange(["-e", pattern, "--", "."]);
        return Proc.Run("rg", a, 60, root.Path);
    }

    // Runs git grep with PCRE.
    private static ProcResult RunGitGrep(Root root, string pattern, bool ignoreCase, string? glob)
    {
        var a = new List<string> { "grep", "-n", "-I", "-P", "--no-color", "--untracked" };
        if (ignoreCase)
            a.Add("-i");
        a.AddRange(["-e", pattern, "--"]);
        a.Add(glob is null ? "." : $":(glob){(glob.Contains('/') ? glob : "**/" + glob)}");
        return Proc.Run("git", a, 60, root.Path);
    }

    // Splits "path:line:text" (ripgrep prefixes "./").
    private static (string file, int line, string text)? Parse(string raw)
    {
        var a = raw.IndexOf(':');
        var b = a < 0 ? -1 : raw.IndexOf(':', a + 1);
        if (b < 0 || !int.TryParse(raw[(a + 1)..b], out var line))
            return null;
        var file = raw[..a];
        return (file.StartsWith("./") ? file[2..] : file, line, raw[(b + 1)..]);
    }
}
