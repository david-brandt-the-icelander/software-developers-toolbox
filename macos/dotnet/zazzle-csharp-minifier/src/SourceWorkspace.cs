// A Roslyn workspace of every .cs file under the root, one project per .csproj, for renaming symbols with all their references.
using System.Text.RegularExpressions;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Host.Mef;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleMinifier;

// The solution built from the tree, with each document's source file so rewrites keep its byte-order mark.
internal sealed partial class SourceWorkspace
{
    // The global usings SDK projects with ImplicitUsings add, so references bind as they do in the real build.
    private const string ImplicitUsings = "global using System; global using System.Collections.Generic; global using System.IO; global using System.Linq; global using System.Net.Http; global using System.Threading; global using System.Threading.Tasks;";

    // The name of the added implicit-usings document, which is never written back.
    public const string ImplicitUsingsName = "<implicit usings>";

    public Solution Solution { get; }
    public Dictionary<string, SourceFile> Files { get; }

    // Wraps a built solution.
    private SourceWorkspace(Solution solution, Dictionary<string, SourceFile> files)
    {
        Solution = solution;
        Files = files;
    }

    // Loads every readable .cs file under the root into projects that follow the .csproj files.
    public static SourceWorkspace Load(string root)
    {
        var workspace = new AdhocWorkspace(MefHostServices.Create(MefHostServices.DefaultAssemblies));
        var files = new Dictionary<string, SourceFile>(StringComparer.Ordinal);
        var groups = new Dictionary<string, List<SourceFile>>(StringComparer.Ordinal);
        foreach (var path in SourceFiles.Under([root]))
        {
            var file = SourceFile.Read(root, path, out _);
            if (file == null)
                continue;
            files[path] = file;
            var key = Layout.ProjectDir(root, path) ?? root;
            if (!groups.TryGetValue(key, out var list))
                groups[key] = list = [];
            list.Add(file);
        }
        var ids = groups.Keys.ToDictionary(k => k, _ => ProjectId.CreateNewId(), StringComparer.Ordinal);
        var solution = workspace.CurrentSolution;
        foreach (var (dir, list) in groups)
            solution = AddProject(solution, root, dir, list, ids);
        return new SourceWorkspace(solution, files);
    }

    // Adds one project with its documents, references and parse options.
    private static Solution AddProject(Solution solution, string root, string dir, List<SourceFile> files, Dictionary<string, ProjectId> ids)
    {
        var csproj = Directory.EnumerateFiles(dir, "*.csproj").Order(StringComparer.Ordinal).FirstOrDefault();
        var name = Path.GetRelativePath(root, csproj ?? dir).Replace('\\', '/');
        var version = ProjectLanguage.For(root, Path.Combine(dir, "x.cs")).Version ?? LanguageVersion.Preview;
        var options = new CSharpParseOptions(version);
        var id = ids[dir];
        var info = ProjectInfo.Create(id, VersionStamp.Create(), name, AssemblyName().Replace(name, "_"), LanguageNames.CSharp,
            parseOptions: options,
            compilationOptions: new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, nullableContextOptions: NullableContextOptions.Enable),
            metadataReferences: Basic.Reference.Assemblies.Net80.References.All,
            projectReferences: csproj == null ? [] : References(csproj, ids).Select(r => new ProjectReference(r)));
        solution = solution.AddProject(info);
        foreach (var file in files)
            solution = solution.AddDocument(DocumentId.CreateNewId(id), file.Rel, SourceText.From(file.Text), filePath: file.Path);
        if (csproj != null && UsesImplicitUsings(root, csproj))
            solution = solution.AddDocument(DocumentId.CreateNewId(id), ImplicitUsingsName, SourceText.From(ImplicitUsings));
        return solution;
    }

    // The projects this .csproj references that are part of the tree.
    private static IEnumerable<ProjectId> References(string csproj, Dictionary<string, ProjectId> ids)
    {
        var dir = Path.GetDirectoryName(csproj)!;
        foreach (Match match in ProjectReferenceInclude().Matches(File.ReadAllText(csproj)))
        {
            var target = Path.GetDirectoryName(Path.GetFullPath(Path.Combine(dir, match.Groups[1].Value.Replace('\\', '/'))))!;
            if (ids.TryGetValue(target, out var id))
                yield return id;
        }
    }

    // True when the project or a Directory.Build.props above it, up to the root, turns on ImplicitUsings.
    private static bool UsesImplicitUsings(string root, string csproj)
    {
        if (ImplicitUsingsOn().IsMatch(File.ReadAllText(csproj)))
            return true;
        for (var dir = Path.GetDirectoryName(csproj); dir != null && dir.Length >= root.Length; dir = Path.GetDirectoryName(dir))
        {
            var props = Path.Combine(dir, "Directory.Build.props");
            if (File.Exists(props))
                return ImplicitUsingsOn().IsMatch(File.ReadAllText(props));
        }
        return false;
    }

    // Counts each compiler error id in every project, the measure a rename must not raise.
    public static async Task<Dictionary<string, int>> ErrorCounts(Solution solution)
    {
        var counts = new Dictionary<string, int>(StringComparer.Ordinal);
        foreach (var project in solution.Projects)
        {
            var compilation = await project.GetCompilationAsync();
            foreach (var d in compilation!.GetDiagnostics().Where(d => d.Severity == DiagnosticSeverity.Error))
                counts[$"{project.Name}:{d.Id}"] = counts.GetValueOrDefault($"{project.Name}:{d.Id}") + 1;
        }
        return counts;
    }

    // Characters not allowed in the assembly name made from a project's path.
    [GeneratedRegex("[^A-Za-z0-9_]")]
    private static partial Regex AssemblyName();

    // A <ProjectReference Include="..."> element.
    [GeneratedRegex("<ProjectReference\\s+Include=\"([^\"]+)\"")]
    private static partial Regex ProjectReferenceInclude();

    // An <ImplicitUsings> element set to enable or true.
    [GeneratedRegex("<ImplicitUsings>\\s*(enable|true)\\s*</ImplicitUsings>", RegexOptions.IgnoreCase)]
    private static partial Regex ImplicitUsingsOn();
}
