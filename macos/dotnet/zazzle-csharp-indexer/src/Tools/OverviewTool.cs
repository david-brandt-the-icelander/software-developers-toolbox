// Tool that summarises a root: languages, layout, entry points and largest files.
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Tools;

// Computes everything from the index without touching file contents.
public sealed class OverviewTool(RootSet roots) : ITool
{
    private static readonly string[] EntryNames = ["Program.cs", "main.rs", "main.go", "main.py", "index.ts", "index.js", "app.py", "README.md", "AGENTS.md", "CLAUDE.md", "Cargo.toml", "package.json", "go.mod", "pyproject.toml", "Makefile"];

    public string Name => "overview";

    // Builds the overview text for each selected root.
    public string Run(JsonNode? args, CallInfo info)
    {
        info.Path = "index";
        var parts = roots.Select(Args.OptStr(args, "root")).Select(r => Describe(r, info)).ToList();
        return string.Join("\n\n", parts);
    }

    // How long overview waits for a cold index before answering with what is analysed so far.
    private const int ColdWaitMs = 3000;

    // Describes one root: counts honour its `.ignore`, projects are split into production and test, and a cold index gives a partial answer.
    private static string Describe(Root root, CallInfo info)
    {
        var ready = root.Index.WaitReady(ColdWaitMs);
        var ignore = IgnoreRules.Load(root.Path);
        var all = root.Index.Files().Where(f => f.BlobId != -1).ToList();
        var files = all.Where(f => !ignore.Ignores(f.Path)).Select(f => (f.Path, meta: root.Index.Engine.Blob(f.BlobId))).ToList();
        info.TotalFiles += files.Count;
        var langs = files.Where(f => f.meta is { Binary: false }).GroupBy(f => f.meta!.Lang)
            .Select(g => (lang: g.Key, n: g.Count(), lines: g.Sum(x => x.meta!.Lines))).OrderByDescending(x => x.lines).Take(6);
        var dirs = files.GroupBy(f => f.Path.Contains('/') ? f.Path[..f.Path.IndexOf('/')] + "/" : "(root)").OrderByDescending(g => g.Count()).Take(10);
        var entries = files.Where(f => f.Path.Count(c => c == '/') <= 1 && EntryNames.Contains(System.IO.Path.GetFileName(f.Path)) && !IsAgentImport(f.Path, files.Select(x => x.Path))).Select(f => f.Path).OrderBy(p => p.Count(c => c == '/')).Take(6);
        var largest = files.Where(f => f.meta is { Binary: false }).OrderByDescending(f => f.meta!.Lines).Take(5).Select(f => $"{f.Path} ({f.meta!.Lines})");
        var lines = new List<string>
        {
            $"{root.Name}: {files.Count} files" + (all.Count > files.Count ? $" ({all.Count - files.Count} more hidden by .ignore)" : "") + (ready ? "" : "; index still building, counts are partial"),
            "languages: " + string.Join(", ", langs.Select(l => $"{l.lang} {l.n} files/{l.lines} lines")),
            "dirs: " + string.Join(", ", dirs.Select(d => $"{d.Key} {d.Count()}")),
        };
        lines.AddRange(Projects(files.Select(f => f.Path)));
        lines.Add("entry files: " + string.Join(", ", entries) + SolutionNote(files.Select(f => f.Path)));
        lines.Add("largest: " + string.Join(", ", largest));
        return string.Join('\n', lines);
    }

    // Lists project (.csproj) files: production ones by name, test ones grouped by their name prefix.
    private static IEnumerable<string> Projects(IEnumerable<string> paths)
    {
        var projects = paths.Where(p => p.EndsWith(".csproj", StringComparison.OrdinalIgnoreCase)).Select(p => (path: p, name: System.IO.Path.GetFileNameWithoutExtension(p))).ToList();
        if (projects.Count == 0)
            yield break;
        var tests = projects.Where(p => IsTestProject(p)).ToList();
        var prod = projects.Where(p => !IsTestProject(p)).ToList();
        yield return $"projects: {prod.Count} production" + (prod.Count > 0 ? ": " + string.Join(", ", prod.Take(10).Select(p => p.name)) + (prod.Count > 10 ? $", +{prod.Count - 10} more" : "") : "");
        if (tests.Count > 0)
            yield return $"test projects: {tests.Count}: " + string.Join(", ", tests.GroupBy(t => t.name.Split('.')[0]).OrderByDescending(g => g.Count()).Take(8).Select(g => $"{g.Key}.* {g.Count()}"));
    }

    // True for a project named or placed as a test project.
    private static bool IsTestProject((string path, string name) p) =>
        p.name.Contains("Test", StringComparison.OrdinalIgnoreCase) || p.path.Split('/').Any(seg => seg.Contains("Test", StringComparison.OrdinalIgnoreCase));

    // True for a CLAUDE.md that sits beside an AGENTS.md (it only imports it).
    private static bool IsAgentImport(string path, IEnumerable<string> all) =>
        System.IO.Path.GetFileName(path) == "CLAUDE.md" && all.Contains(path[..^"CLAUDE.md".Length] + "AGENTS.md");

    // Names the solution files, if any.
    private static string SolutionNote(IEnumerable<string> paths)
    {
        var sln = paths.Where(p => p.EndsWith(".sln", StringComparison.OrdinalIgnoreCase) || p.EndsWith(".slnx", StringComparison.OrdinalIgnoreCase)).Take(6).ToList();
        return sln.Count == 0 ? "" : "; solutions: " + string.Join(", ", sln);
    }
}
