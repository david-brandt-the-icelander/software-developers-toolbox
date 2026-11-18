// Tool that lists exact references to a C# symbol with Roslyn's SymbolFinder, so zazzle refs need not match text.
using System.Text.Json.Nodes;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.FindSymbols;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleIndexer.Tools;

// Keeps one in-memory solution per root, rebuilt when the index generation changes, and asks SymbolFinder for the references of every declaration with the name.
public sealed class RefsTool(RootSet roots) : ITool
{
    public string Name => "refs";

    private const int MaxFiles = 20000;
    private const int MaxLine = 160;
    private const string GlobalUsings = "global using System; global using System.Collections.Generic; global using System.IO; global using System.Linq; global using System.Net.Http; global using System.Threading; global using System.Threading.Tasks;";
    private static readonly Lazy<MetadataReference[]> Framework = new(LoadFramework);
    private readonly Dictionary<string, (long gen, Solution sln)> _cache = [];

    // Finds the declarations matching the name (Type.Member narrows by container) and lists their definitions and references, grouped by file.
    public string Run(JsonNode? args, CallInfo info)
    {
        var query = Args.Str(args, "name").Replace("::", ".");
        var cut = query.LastIndexOf('.');
        var name = cut < 0 ? query : query[(cut + 1)..];
        var qualifier = cut < 0 ? "" : query[..cut];
        var limit = Args.Int(args, "limit", 50);
        info.Path = "roslyn";
        var defs = new SortedSet<string>(StringComparer.Ordinal);
        var refs = new SortedDictionary<string, SortedDictionary<int, string>>(StringComparer.Ordinal);
        foreach (var root in roots.Select(Args.OptStr(args, "root")))
            Collect(root, name, qualifier, defs, refs);
        if (defs.Count == 0)
            throw new ToolException("not_found", $"no C# declaration of {query}");
        var total = refs.Values.Sum(r => r.Count);
        var lines = new List<string> { "defs" };
        lines.AddRange(defs);
        lines.Add("refs");
        var shown = 0;
        foreach (var (path, uses) in refs)
        {
            if (shown >= limit)
                break;
            lines.Add(path);
            foreach (var (line, text) in uses.Take(limit - shown))
            {
                lines.Add($"{line} {text}");
                shown++;
            }
        }
        info.Results = shown;
        info.Truncated = shown < total;
        lines.Add($"-- exact (Roslyn): {defs.Count} defs, {total} refs in {refs.Count} files{(info.Truncated ? $"; showing {shown}" : "")}");
        return string.Join('\n', lines);
    }

    // Adds one root's definitions and references of the matching declarations.
    private void Collect(Root root, string name, string qualifier, SortedSet<string> defs, SortedDictionary<string, SortedDictionary<int, string>> refs)
    {
        if (!root.Index.WaitReady(Engine.ReadyTimeoutMs))
            throw new ToolException("index_cold", "index is still building");
        var sln = Solution(root);
        var compilation = sln.Projects.First().GetCompilationAsync().GetAwaiter().GetResult()!;
        var symbols = compilation.GetSymbolsWithName(n => n == name, SymbolFilter.TypeAndMember).Where(s => InContainer(s, qualifier)).ToList();
        foreach (var symbol in symbols)
        {
            foreach (var found in SymbolFinder.FindReferencesAsync(symbol, sln).GetAwaiter().GetResult())
            {
                foreach (var loc in found.Definition.Locations.Where(l => l.IsInSource))
                    defs.Add($"{Rel(root, loc)}:{loc.GetLineSpan().StartLinePosition.Line + 1} {Kind(found.Definition)} {found.Definition.ToDisplayString(SymbolDisplayFormat.CSharpShortErrorMessageFormat)}");
                foreach (var r in found.Locations)
                {
                    var span = r.Location.GetLineSpan();
                    var line = span.StartLinePosition.Line;
                    var text = r.Location.SourceTree!.GetText().Lines[line].ToString().Trim();
                    var file = refs.TryGetValue(Rel(root, r.Location), out var f) ? f : refs[Rel(root, r.Location)] = [];
                    file[line + 1] = text.Length > MaxLine ? text[..MaxLine] + "…" : text;
                }
            }
        }
    }

    // True when the symbol's container ends with the qualifier, or there is none.
    private static bool InContainer(ISymbol s, string qualifier) =>
        qualifier == "" || s.ContainingSymbol is { } c && (c.ToDisplayString() == qualifier || c.ToDisplayString().EndsWith("." + qualifier, StringComparison.Ordinal));

    // Names the kind of a declaration the way find_symbol does.
    private static string Kind(ISymbol s) => s switch
    {
        INamedTypeSymbol t => t.TypeKind.ToString().ToLowerInvariant(),
        IMethodSymbol => "method",
        IPropertySymbol => "property",
        IFieldSymbol => "field",
        IEventSymbol => "event",
        _ => s.Kind.ToString().ToLowerInvariant(),
    };

    // Returns `root:path` for a source location.
    private static string Rel(Root root, Location loc) =>
        $"{root.Name}:{System.IO.Path.GetRelativePath(root.Path, loc.SourceTree!.FilePath).Replace('\\', '/')}";

    // Returns the root's solution, building it again when the index has changed since.
    private Solution Solution(Root root)
    {
        var gen = root.Index.Gen;
        if (_cache.TryGetValue(root.Path, out var hit) && hit.gen == gen)
            return hit.sln;
        var files = root.Index.Files().Where(f => f.Path.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)).ToList();
        if (files.Count > MaxFiles)
            throw new ToolException("too_large", $"{files.Count} C# files is more than {MaxFiles}");
        var ws = new AdhocWorkspace();
        var id = ProjectId.CreateNewId();
        var info = ProjectInfo.Create(id, VersionStamp.Create(), root.Name, root.Name, LanguageNames.CSharp,
            compilationOptions: new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary),
            parseOptions: new CSharpParseOptions(LanguageVersion.Preview),
            metadataReferences: Framework.Value);
        var sln = ws.CurrentSolution.AddProject(info);
        sln = sln.AddDocument(DocumentId.CreateNewId(id), "GlobalUsings.g.cs", SourceText.From(GlobalUsings), filePath: System.IO.Path.Combine(root.Path, "GlobalUsings.g.cs"));
        foreach (var f in files)
        {
            var full = System.IO.Path.Combine(root.Path, f.Path);
            if (File.Exists(full))
                sln = sln.AddDocument(DocumentId.CreateNewId(id), f.Path, SourceText.From(File.ReadAllText(full)), filePath: full);
        }
        _cache[root.Path] = (gen, sln);
        return sln;
    }

    // Returns the framework assemblies the runtime was started with, or none in a single-file build, where sources still resolve among themselves.
    private static MetadataReference[] LoadFramework() =>
        (AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES") as string ?? "")
            .Split(System.IO.Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries)
            .Where(p => System.IO.Path.GetFileName(p) is var n && (n.StartsWith("System.", StringComparison.Ordinal) || n is "System.dll" or "mscorlib.dll" or "netstandard.dll") && File.Exists(p))
            .Select(p => (MetadataReference)MetadataReference.CreateFromFile(p))
            .ToArray();
}
