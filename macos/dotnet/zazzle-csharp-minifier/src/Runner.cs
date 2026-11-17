// Runs the chosen commands in order, applying each plan and setting up the repository or, with --check, reporting the plans as violations.
namespace ZazzleMinifier;

// Chains the commands so each one sees the files the one before it wrote.
internal static class Runner
{
    // Returns 0 when everything went well (and, with --check, nothing would change), 1 otherwise.
    public static int Run(ParsedArgs args, TextWriter output, TextWriter errors)
    {
        if (args.Benchmark != null)
            return Benchmark.Run(args, output, errors);
        if (args.ListIdentifiers)
        {
            output.Write(Identifiers.List(args.Root, Sources(args.Paths)));
            return 0;
        }
        var steps = new List<(string Name, Func<string, IReadOnlyList<string>, Plan> Make)>();
        if (args.RenameFile is { } renameFile)
            steps.Add(("--rename", (root, _) => Rename.Make(root, renameFile)));
        if (args.AllowReads)
            steps.Add(("--allow-reads", (root, _) => RepoSetup.AllowReads(root)));
        if (args.SplitTests)
            steps.Add(("--split-tests", SplitTests.Make));
        var paths = args.Paths;
        var failed = 0;
        var violations = 0;
        foreach (var (name, make) in steps)
        {
            var files = Sources(paths);
            var plan = make(args.Root, files);
            plan.Notes.ForEach(output.WriteLine);
            plan.Errors.ForEach(errors.WriteLine);
            failed += plan.Errors.Count;
            if (args.Check)
            {
                plan.Changes.ForEach(c => output.WriteLine($"{name} would change {c}"));
                violations += plan.Changes.Count;
                continue;
            }
            plan.Apply();
            plan.Changes.ForEach(output.WriteLine);
            paths = [.. paths, .. plan.WrittenSources()];
        }
        // --allow-reads alone touches only .claude/settings.json; the layout commands also bring the skill, AGENTS.md section and hook.
        if (!args.Check && args.CommandSwitches().Any(s => s != "--allow-reads"))
            RepoSetup.Run(args.Root, args.CommandSwitches()).ForEach(f => output.WriteLine($"set up {f}"));
        if (args.Check && violations > 0)
            errors.WriteLine($"{violations} files need changes: run zazzle-csharp-minifier {string.Join(" ", args.CommandSwitches())} to make them");
        return failed > 0 || violations > 0 ? 1 : 0;
    }

    // The source files under the paths that still exist.
    private static List<string> Sources(IEnumerable<string> paths) =>
        SourceFiles.Under(paths.Where(Path.Exists));
}
