// Tool that runs an allowed read-only command in a root, so unforeseen questions still go through the server and get logged.
using System.Text.Json.Nodes;

namespace CodeIndexd.Tools;

// Passthrough for grep, sed, find, git and friends; prefer the dedicated tools.
public sealed class RunTool(RootSet roots) : ITool
{
    private static readonly HashSet<string> Replaceable = ["grep", "egrep", "fgrep", "rg", "find", "cat", "head", "tail", "sed"];

    public string Name => "run";
    public string Description => "Run a read-only command (grep, rg, sed, awk, find, cat, head, tail, wc, ls, git log/blame/show/diff/status ...) in a root. No shell: pass argv as an array, e.g. [\"git\",\"log\",\"-5\"]. Use only when search, read, find_files, outline, find_symbol or find_references cannot answer; use report_gap to say what was missing.";
    public JsonNode Schema
    {
        get
        {
            var s = Args.Schema(["argv"], ("root", "string", "Root to run in (default: the first)"), ("timeout", "integer", "Seconds (default 30, max 120)"), ("max_tokens", "integer", "Output budget (default 4000)"));
            s["properties"]!["argv"] = new JsonObject { ["type"] = "array", ["items"] = new JsonObject { ["type"] = "string" }, ["description"] = "Program and arguments" };
            return s;
        }
    }

    // Checks the policy, runs the command in the root and returns capped output.
    public string Run(JsonNode? args, CallInfo info)
    {
        var argv = args?["argv"]?.AsArray().Select(a => a!.GetValue<string>()).ToList() ?? throw new ToolException("bad_args", "missing argument: argv");
        var root = roots.Select(Args.OptStr(args, "root")).First();
        var stateDir = root.Index.Engine.StateDir;
        if (RunPolicy.Check(argv, RunPolicy.Unrestricted(stateDir)) is { } why)
            throw new ToolException("run_denied", why);
        var prog = Path.GetFileName(argv[0]);
        info.Path = "run:" + prog;
        info.Reason = "passthrough";
        var timeout = Math.Clamp(Args.Int(args, "timeout", 30), 1, 120);
        var r = Proc.Run(argv[0], argv.Skip(1), timeout, root.Path);
        var output = new Output(Args.Int(args, "max_tokens", 4000));
        foreach (var line in r.Stdout.Split('\n').SkipLast(r.Stdout.EndsWith('\n') ? 1 : 0))
            if (!output.Add(line))
            {
                info.Truncated = true;
                break;
            }
        var tail = new List<string>();
        if (r.TimedOut)
            tail.Add($"[timed out after {timeout}s]");
        else if (r.ExitCode != 0)
            tail.Add($"[exit {r.ExitCode}] {r.Stderr.Trim()}");
        if (info.Truncated)
            tail.Add("[output truncated]");
        if (Replaceable.Contains(prog))
            tail.Add("[hint: search, find_files and read answer this from the index with fewer tokens]");
        return output.Finish(string.Join('\n', tail));
    }
}
