// Decides which external commands the run tool may execute.
using System.Text.RegularExpressions;

namespace CodeIndexd.Tools;

// Default policy is a read-only allowlist; the user can lift it in config.json. It guards against agent mistakes, not hostile agents.
public static class RunPolicy
{
    private static readonly HashSet<string> Programs =
    [
        "grep", "egrep", "fgrep", "rg", "find", "sed", "awk", "cat", "head", "tail", "wc", "ls", "sort", "uniq", "diff",
        "cut", "tr", "file", "stat", "du", "nl", "git", "basename", "dirname", "tree",
    ];

    private static readonly HashSet<string> GitReadOnly =
    [
        "log", "blame", "show", "diff", "status", "ls-files", "grep", "rev-parse", "cat-file", "ls-tree", "describe", "shortlog", "rev-list", "branch", "tag", "name-rev", "merge-base", "diff-tree",
    ];

    // Reads the unrestricted switch from the environment or config.json in the state directory.
    public static bool Unrestricted(string stateDir)
    {
        if (Environment.GetEnvironmentVariable("CODEINDEXD_RUN_UNRESTRICTED") == "1")
            return true;
        var file = Path.Combine(stateDir, "config.json");
        if (!File.Exists(file))
            return false;
        try
        {
            return System.Text.Json.Nodes.JsonNode.Parse(File.ReadAllText(file))?["run"]?["unrestricted"]?.GetValue<bool>() == true;
        }
        catch (System.Text.Json.JsonException)
        {
            return false;
        }
    }

    // Returns null when the command may run, otherwise the reason it may not.
    public static string? Check(IReadOnlyList<string> argv, bool unrestricted)
    {
        if (argv.Count == 0)
            return "empty command";
        if (unrestricted)
            return null;
        var prog = Path.GetFileName(argv[0]);
        if (!Programs.Contains(prog))
            return $"{prog} is not in the read-only allowlist ({string.Join(", ", Programs.Order())}); set run.unrestricted in config.json to lift this";
        var rest = argv.Skip(1).ToList();
        return prog switch
        {
            "sed" when rest.Any(a => Regex.IsMatch(a, @"^-[A-Za-z]*i") || a.StartsWith("--in-place")) => "sed -i would edit files",
            "sed" when rest.Any(a => Regex.IsMatch(a, @"(^|[;{\s])[wWe]\s*\S|/[wWe]\s*\S*$")) => "sed w/e commands are not allowed",
            "awk" when rest.Any(a => a.Contains("system(") || Regex.IsMatch(a, @">\s*""|\|\s*""|>>")) => "awk output redirection and system() are not allowed",
            "find" when rest.Any(a => a is "-delete" or "-exec" or "-execdir" or "-ok" or "-okdir" or "-fprint" or "-fls" or "-fprintf") => "find actions that modify or run commands are not allowed",
            "rg" when rest.Any(a => a.StartsWith("--pre")) => "rg --pre runs commands",
            "git" => CheckGit(rest),
            _ => null,
        };
    }

    // Allows only read-only git subcommands without output-writing options.
    private static string? CheckGit(List<string> rest)
    {
        var sub = rest.FirstOrDefault(a => !a.StartsWith('-') && a != "-C");
        if (rest.Any(a => a == "-c" || a.StartsWith("--output") || a.StartsWith("--exec-path")))
            return "git -c and --output are not allowed";
        return sub is null || !GitReadOnly.Contains(sub) ? $"git {sub} is not a read-only subcommand" : null;
    }
}
