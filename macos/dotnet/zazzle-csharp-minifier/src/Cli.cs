// Parses the command-line switches into what to run and on which paths.
namespace ZazzleMinifier;

// The parsed command line; Error is set when it cannot be run.
internal sealed record ParsedArgs(
    bool Help, bool Version, bool Strip, bool SplitTests, bool SplitHeaders, bool Check,
    string Root, List<string> Paths, string? Error,
    bool Restructure = false, bool ListIdentifiers = false, string? RenameFile = null)
{
    // The command switches given, in the order they run, as typed on the command line.
    public List<string> CommandSwitches() =>
        [.. new[] { (Strip, "--strip-comments"), (SplitTests, "--split-tests"), (SplitHeaders, "--split-headers"), (Restructure, "--restructure") }.Where(s => s.Item1).Select(s => s.Item2)];
}

// Turns args into ParsedArgs and checks that the paths exist inside the root.
internal static class Cli
{
    // Parses args relative to the current directory.
    public static ParsedArgs Parse(string[] args)
    {
        bool help = false, version = false, strip = false, splitTests = false, splitHeaders = false, check = false, restructure = false, list = false;
        string? root = null, rename = null;
        var paths = new List<string>();
        for (var i = 0; i < args.Length; i++)
        {
            switch (args[i])
            {
                case "--help" or "-h": help = true; break;
                case "--version": version = true; break;
                case "--strip-comments": strip = true; break;
                case "--split-tests": splitTests = true; break;
                case "--split-headers": splitHeaders = true; break;
                case "--check": check = true; break;
                case "--restructure": restructure = true; break;
                case "--list-identifiers": list = true; break;
                case "--rename" when i + 1 < args.Length: rename = args[++i]; break;
                case "--root" when i + 1 < args.Length: root = args[++i]; break;
                case var a when a.StartsWith('-'): return Fail($"unknown or incomplete switch: {a}");
                default: paths.Add(args[i]); break;
            }
        }
        // The restructured layout is built from the three other commands' output.
        if (restructure)
            strip = splitTests = splitHeaders = true;
        if (help || version)
            return new ParsedArgs(help, version, false, false, false, false, "", paths, null);
        if (!strip && !splitTests && !splitHeaders && !restructure && !list && rename == null)
            return Fail("nothing to do: give --strip-comments, --split-tests, --split-headers, --restructure, --list-identifiers or --rename");
        if (list && (strip || splitTests || splitHeaders || restructure || rename != null || check))
            return Fail("--list-identifiers runs on its own");
        if (rename != null && check)
            return Fail("--rename cannot be checked");
        if (rename != null && !File.Exists(rename))
            return Fail($"rename file not found: {rename}");
        var fullRoot = Path.GetFullPath(root ?? Directory.GetCurrentDirectory());
        if (!Directory.Exists(fullRoot))
            return Fail($"root is not a folder: {fullRoot}");
        var fullPaths = paths.Count == 0 ? [fullRoot] : paths.Select(p => Path.GetFullPath(p)).ToList();
        var bad = fullPaths.FirstOrDefault(p => !Inside(fullRoot, p) || !Path.Exists(p));
        if (bad != null)
            return Fail($"path is missing or outside the root: {bad}");
        return new ParsedArgs(false, false, strip, splitTests, splitHeaders, check, fullRoot, fullPaths, null,
            restructure, list, rename == null ? null : Path.GetFullPath(rename));
    }

    // True when path is the root or below it.
    private static bool Inside(string root, string path)
    {
        var rel = Path.GetRelativePath(root, path);
        return rel == "." || (!rel.StartsWith("..", StringComparison.Ordinal) && !Path.IsPathRooted(rel));
    }

    // A ParsedArgs that carries only an error.
    private static ParsedArgs Fail(string error) => new(false, false, false, false, false, false, "", [], error);
}
