// Tool that summarises a root: languages, layout, entry points and largest files.
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Tools;

// Computes everything from the index without touching file contents.
public sealed class OverviewTool(RootSet roots) : ITool
{
    private static readonly string[] EntryNames = ["Program.cs", "main.rs", "main.go", "main.py", "index.ts", "index.js", "app.py", "README.md", "AGENTS.md", "CLAUDE.md", "Cargo.toml", "package.json", "go.mod", "pyproject.toml", "Makefile"];

    public string Name => "overview";
    public string Description => "Compact map of a root: file and line counts by language, top-level directories, entry points and project files, largest files. Start here on an unfamiliar repo.";
    public JsonNode Schema => Args.Schema([], ("root", "string", "Limit to one root"));

    // Builds the overview text for each selected root.
    public string Run(JsonNode? args, CallInfo info)
    {
        info.Path = "index";
        var parts = roots.Select(Args.OptStr(args, "root")).Select(r => Describe(r, info)).ToList();
        return string.Join("\n\n", parts);
    }

    // Describes one root.
    private static string Describe(Root root, CallInfo info)
    {
        if (!root.Index.WaitReady(Engine.ReadyTimeoutMs))
            throw new ToolException("index_cold", "index is still building; use find_files");
        var files = root.Index.Files().Select(f => (f.Path, meta: root.Index.Engine.Blob(f.BlobId))).ToList();
        info.TotalFiles += files.Count;
        var langs = files.Where(f => f.meta is { Binary: false }).GroupBy(f => f.meta!.Lang)
            .Select(g => (lang: g.Key, n: g.Count(), lines: g.Sum(x => x.meta!.Lines))).OrderByDescending(x => x.lines).Take(8);
        var dirs = files.GroupBy(f => f.Path.Contains('/') ? f.Path[..f.Path.IndexOf('/')] + "/" : "(root)").OrderByDescending(g => g.Count()).Take(12);
        var entries = files.Where(f => EntryNames.Contains(System.IO.Path.GetFileName(f.Path)) || f.Path.EndsWith(".csproj") || f.Path.EndsWith(".sln")).Select(f => f.Path).Take(15);
        var largest = files.Where(f => f.meta is { Binary: false }).OrderByDescending(f => f.meta!.Lines).Take(8).Select(f => $"{f.Path} ({f.meta!.Lines})");
        return string.Join('\n',
        [
            $"{root.Name}: {files.Count} files",
            "languages: " + string.Join(", ", langs.Select(l => $"{l.lang} {l.n} files/{l.lines} lines")),
            "dirs: " + string.Join(", ", dirs.Select(d => $"{d.Key} {d.Count()}")),
            "entry/project files: " + string.Join(", ", entries),
            "largest: " + string.Join(", ", largest),
        ]);
    }
}
