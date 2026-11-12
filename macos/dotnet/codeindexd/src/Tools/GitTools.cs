// Tools for commit history and line authorship, answered by git and kept compact.
using System.Text.Json.Nodes;

namespace CodeIndexd.Tools;

// Short commit list, optionally for one path.
public sealed class GitLogTool(RootSet roots) : ITool
{
    public string Name => "git_log";
    public string Description => "Recent commits as 'hash date author subject', optionally for one path (root:path), with an optional message filter. Replaces git log.";
    public JsonNode Schema => Args.Schema([], ("path", "string", "root:path to limit history"), ("root", "string", "Root when no path is given"), ("n", "integer", "Number of commits (default 20)"), ("grep", "string", "Only commits whose message matches"));

    // Runs git log with a compact format.
    public string Run(JsonNode? args, CallInfo info)
    {
        info.Path = "run:git";
        info.Reason = "git";
        var (root, rel) = Args.OptStr(args, "path") is { } p ? roots.Split(p) : (roots.Select(Args.OptStr(args, "root")).First(), "");
        var cmd = new List<string> { "-C", root.Path, "log", $"-{Args.Int(args, "n", 20)}", "--date=short", "--format=%h %ad %an %s" };
        if (Args.OptStr(args, "grep") is { } g)
            cmd.AddRange(["--grep", g, "-i"]);
        if (rel != "")
            cmd.AddRange(["--", rel]);
        var r = Proc.Run("git", cmd, 30);
        if (r.ExitCode != 0)
            throw new ToolException("git_failed", r.Stderr.Trim());
        info.Results = r.Stdout.Count(c => c == '\n');
        return r.Stdout.TrimEnd();
    }
}

// Line authorship for a range of a file.
public sealed class GitBlameTool(RootSet roots) : ITool
{
    public string Name => "git_blame";
    public string Description => "Who last changed each line of a range: 'hash author date line text'. Replaces git blame.";
    public JsonNode Schema => Args.Schema(["path"], ("path", "string", "root:path"), ("start_line", "integer", "First line (default 1)"), ("end_line", "integer", "Last line (default start+39)"));

    // Runs git blame for the range.
    public string Run(JsonNode? args, CallInfo info)
    {
        info.Path = "run:git";
        info.Reason = "git";
        var (root, rel) = roots.Split(Args.Str(args, "path"));
        var start = Math.Max(1, Args.Int(args, "start_line", 1));
        var end = Args.Int(args, "end_line", start + 39);
        var r = Proc.Run("git", ["-C", root.Path, "blame", "-w", "--date=short", "--abbrev=7", "-L", $"{start},{end}", "--", rel], 30);
        if (r.ExitCode != 0)
            throw new ToolException("git_failed", r.Stderr.Trim());
        info.Results = end - start + 1;
        return r.Stdout.TrimEnd();
    }
}
