// Decides whether a Claude Code tool call should be redirected to the zazzle-csharp-indexer tools.
using System.Text;
using System.Text.RegularExpressions;

namespace ZazzleIndexer.Cli;

// The outcome for one tool call; Program is the matched command name for the usage log.
public sealed record HookDecision(bool Block, string Message, string Program);

// Static policy used by the `hook` command.
public static class HookPolicy
{
    private const string AllowMarker = "# allow-raw";
    private static readonly HashSet<string> Wrappers = ["sudo", "command", "exec", "nohup", "time", "env", "xargs", "builtin"];
    private static readonly HashSet<string> Searchers = ["grep", "egrep", "fgrep", "rg", "ag", "ack", "zgrep"];
    private static readonly HashSet<string> Readers = ["cat", "head", "tail", "less", "more", "bat", "nl"];
    private static readonly HashSet<string> FindActions = ["-delete", "-exec", "-execdir", "-ok", "-okdir"];

    private const string SearchMsg = "Use the zazzle-csharp-indexer MCP tool `search` (mcp__zazzle-csharp-indexer__search) instead of grep/rg: it is index-accelerated and cheaper in tokens (args: pattern, literal, glob, context, mode=files|count|lines).";
    private const string FindMsg = "Use the zazzle-csharp-indexer MCP tool `find_files` (mcp__zazzle-csharp-indexer__find_files) instead of find/Glob (args: query, glob).";
    private const string ReadMsg = "Use the zazzle-csharp-indexer MCP tool `read` (mcp__zazzle-csharp-indexer__read, args: path root:file, start_line, end_line) or `outline` instead of cat/head/tail/sed -n.";
    private const string Escape = " If these tools cannot do what you need, call `report_gap`, or add the comment `# allow-raw` to the command (logged).";

    // Evaluates a tool call given Claude Code's tool name and its tool_input command (for Bash).
    public static HookDecision Decide(string toolName, string? command)
    {
        if (Environment.GetEnvironmentVariable("ZAZZLE_INDEXER_HOOK") == "off")
            return new(false, "", "");
        if (toolName == "Grep")
            return new(true, SearchMsg + Escape, "Grep");
        if (toolName == "Glob")
            return new(true, FindMsg + Escape, "Glob");
        if (toolName != "Bash" || command is null)
            return new(false, "", "");
        foreach (var seg in Segments(command))
            if (Classify(seg) is { } hit)
                return command.Contains(AllowMarker) ? new(false, "", "allow-raw:" + hit.Program) : hit with { Message = hit.Message + Escape };
        return new(false, "", "");
    }

    // Looks at one pipeline segment and returns a blocking decision if it searches or reads files.
    private static HookDecision? Classify(List<string> tokens)
    {
        var i = 0;
        while (i < tokens.Count && (Regex.IsMatch(tokens[i], @"^\w+=") || Wrappers.Contains(Path.GetFileName(tokens[i])) || (i > 0 && tokens[i].StartsWith('-') && Wrappers.Contains(Path.GetFileName(tokens[0])))))
            i++;
        if (i >= tokens.Count)
            return null;
        var prog = Path.GetFileName(tokens[i]);
        var args = tokens.Skip(i + 1).ToList();
        var files = args.Where(a => !a.StartsWith('-') && !Regex.IsMatch(a, @"^\d+$") && !a.Contains('>') && !a.Contains('<')).ToList();
        var redirected = tokens.Any(t => t.Contains('>') || t.Contains("<<"));
        if (Searchers.Contains(prog))
            return new(true, SearchMsg, prog);
        if (prog == "git" && args.FirstOrDefault(a => !a.StartsWith('-')) == "grep")
            return new(true, SearchMsg, "git-grep");
        if (prog == "find" && !args.Any(FindActions.Contains))
            return new(true, FindMsg, prog);
        if (Readers.Contains(prog) && files.Count > 0 && !redirected && !(prog == "tail" && args.Any(a => a is "-f" or "-F" or "--follow")))
            return new(true, ReadMsg, prog);
        if (prog == "sed" && args.Any(a => Regex.IsMatch(a, @"^-[A-Za-z]*n")) && !args.Any(a => Regex.IsMatch(a, @"^-[A-Za-z]*i")) && files.Count >= 2)
            return new(true, ReadMsg, prog);
        return null;
    }

    // Splits a shell command into segments of unquoted words at |, ||, &&, ;, & and newlines.
    public static List<List<string>> Segments(string cmd)
    {
        var segs = new List<List<string>>();
        var cur = new List<string>();
        var word = new StringBuilder();
        char quote = '\0';
        void EndWord()
        {
            if (word.Length > 0)
                cur.Add(word.ToString());
            word.Clear();
        }
        void EndSeg()
        {
            EndWord();
            if (cur.Count > 0)
                segs.Add(cur);
            cur = [];
        }
        for (var i = 0; i < cmd.Length; i++)
        {
            var c = cmd[i];
            if (quote != '\0')
            {
                if (c == quote)
                    quote = '\0';
                else if (c == '\\' && quote == '"' && i + 1 < cmd.Length)
                    word.Append(cmd[++i]);
                else
                    word.Append(c);
            }
            else if (c is '\'' or '"')
                quote = c;
            else if (c == '\\' && i + 1 < cmd.Length)
                word.Append(cmd[++i]);
            else if (c == '#' && word.Length == 0)
                break;
            else if (c is '|' or ';' or '&' or '\n' or '(' or ')' or '`')
                EndSeg();
            else if (char.IsWhiteSpace(c))
                EndWord();
            else
                word.Append(c);
        }
        EndSeg();
        return segs;
    }
}
