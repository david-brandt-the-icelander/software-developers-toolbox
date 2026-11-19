// The --rename command: applies "name => newName" lines of an identifier tree deepest first, then moves the files that follow the renamed names.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Rename;

namespace CSharpMinifier;

// What a rename path names: a namespace, a type (with its containing types) or a member (with its type's chain).
internal enum RenameKind { Namespace, Type, Member }

// A rename checked against the code: its request, what it names, and the type chain (outermost first) a type or member sits in.
internal sealed record ResolvedRename(RenameRequest Request, RenameKind Kind, IReadOnlyList<string> TypeChain);

// Plans renames with Roslyn's renamer, refusing any that adds a compiler error, and the file moves and document updates they bring.
internal static class Rename
{
    // Rename every overload, nothing in strings or comments, and leave file names to FileMoves.
    private static readonly SymbolRenameOptions Options = new(RenameOverloads: true, RenameInStrings: false, RenameInComments: false, RenameFile: false);

    // The edits that perform the renames in the rename file.
    public static Plan Make(string root, string renameFile)
    {
        var plan = new Plan();
        var (requests, errors) = IdentifierTree.Parse(File.ReadAllText(renameFile));
        plan.Errors.AddRange(errors);
        if (errors.Count > 0 || requests.Count == 0)
            return plan;
        var workspace = SourceWorkspace.Load(root);
        var resolved = Resolve(workspace.Solution, requests, plan);
        if (plan.Errors.Count > 0)
            return plan;
        var solution = Apply(workspace.Solution, resolved, plan).GetAwaiter().GetResult();
        if (solution == null)
            return plan;
        var texts = ChangedTexts(workspace, solution);
        FileMoves.Plan(root, workspace, resolved, texts, plan);
        return plan;
    }

    // Checks every request names something declared in the tree, and records what kind of thing it is.
    private static List<ResolvedRename> Resolve(Solution solution, List<RenameRequest> requests, Plan plan)
    {
        var resolved = new List<ResolvedRename>();
        foreach (var request in requests)
        {
            var symbol = solution.Projects.Select(p => Find(p, request.Path).GetAwaiter().GetResult()).FirstOrDefault(s => s != null);
            if (symbol == null)
            {
                plan.Errors.Add($"line {request.Line}: {request.Dotted} is not declared in the source");
                continue;
            }
            resolved.Add(new ResolvedRename(request, KindOf(symbol), TypeChain(symbol)));
        }
        return resolved;
    }

    // Namespace, type or member.
    private static RenameKind KindOf(ISymbol symbol) => symbol switch
    {
        INamespaceSymbol => RenameKind.Namespace,
        INamedTypeSymbol => RenameKind.Type,
        _ => RenameKind.Member,
    };

    // The names of the type itself (for a type) or the containing type (for a member) and the types around it, outermost first.
    private static List<string> TypeChain(ISymbol symbol)
    {
        var chain = new List<string>();
        for (var type = symbol as INamedTypeSymbol ?? symbol.ContainingType; type != null; type = type.ContainingType)
            chain.Insert(0, type.Name);
        return chain;
    }

    // Applies the renames deepest first, each until no symbol with the old path is left in any project; null when one fails.
    private static async Task<Solution?> Apply(Solution solution, List<ResolvedRename> renames, Plan plan)
    {
        var before = await SourceWorkspace.ErrorCounts(solution);
        foreach (var rename in renames)
        {
            var (next, files) = await RenameAll(solution, rename.Request);
            if (next == null)
            {
                plan.Errors.Add($"line {rename.Request.Line}: Roslyn could not rename {rename.Request.Dotted}");
                return null;
            }
            solution = next;
            plan.Changes.Add($"renamed {rename.Request.Dotted} => {rename.Request.NewName} ({files} files)");
        }
        var added = NewErrors(before, await SourceWorkspace.ErrorCounts(solution), solution).Take(3).ToList();
        if (added.Count == 0)
            return solution;
        plan.Changes.Clear();
        plan.Errors.Add("renames left unchanged: they would add compiler errors: " + string.Join("; ", added));
        return null;
    }

    // Renames every symbol at the path, project by project, and counts the documents that changed; null solution when the renamer changes nothing.
    private static async Task<(Solution? Solution, int Files)> RenameAll(Solution solution, RenameRequest request)
    {
        var changed = new HashSet<DocumentId>();
        foreach (var id in solution.ProjectIds)
        {
            for (var guard = 0; guard < 100; guard++)
            {
                var symbol = await Find(solution.GetProject(id)!, request.Path);
                if (symbol == null)
                    break;
                var next = await Renamer.RenameSymbolAsync(solution, symbol, Options, request.NewName);
                var docs = next.GetChanges(solution).GetProjectChanges().SelectMany(p => p.GetChangedDocuments()).ToList();
                if (docs.Count == 0)
                    return (null, 0);
                changed.UnionWith(docs);
                solution = next;
            }
        }
        return (solution, changed.Count);
    }

    // The first symbol at the dotted path that this project declares in its own source.
    private static async Task<ISymbol?> Find(Project project, IReadOnlyList<string> path)
    {
        var compilation = await project.GetCompilationAsync();
        IEnumerable<INamespaceOrTypeSymbol> containers = [compilation!.GlobalNamespace];
        var found = new List<ISymbol>();
        foreach (var name in path)
        {
            found = containers.SelectMany(c => c.GetMembers(name)).Where(Nameable).ToList();
            containers = found.OfType<INamespaceOrTypeSymbol>().ToList();
        }
        return found.FirstOrDefault(s => s.Locations.Any(l => l.IsInSource && compilation.ContainsSyntaxTree(l.SourceTree!)));
    }

    // True for symbols that carry a name of their own in the tree: no constructors, accessors, operators or indexers.
    private static bool Nameable(ISymbol symbol) => !symbol.IsImplicitlyDeclared && symbol switch
    {
        IMethodSymbol m => m.MethodKind == MethodKind.Ordinary,
        IPropertySymbol p => !p.IsIndexer,
        _ => true,
    };

    // The error ids whose count went up, with one message each.
    private static IEnumerable<string> NewErrors(Dictionary<string, int> before, Dictionary<string, int> after, Solution solution)
    {
        foreach (var (key, count) in after)
            if (count > before.GetValueOrDefault(key))
                yield return $"{key} ({FirstMessage(solution, key)})";
    }

    // The first message of an error id in a project, for the report.
    private static string FirstMessage(Solution solution, string key)
    {
        var split = key.LastIndexOf(':');
        var project = solution.Projects.First(p => p.Name == key[..split]);
        var compilation = project.GetCompilationAsync().GetAwaiter().GetResult()!;
        var diagnostic = compilation.GetDiagnostics().First(d => d.Id == key[(split + 1)..] && d.Severity == DiagnosticSeverity.Error);
        var span = diagnostic.Location.GetLineSpan();
        return $"{Path.GetFileName(span.Path)}:{span.StartLinePosition.Line + 1}: {diagnostic.GetMessage()}";
    }

    // The new text of every source file the renames changed, by path.
    private static Dictionary<string, string> ChangedTexts(SourceWorkspace workspace, Solution solution)
    {
        var texts = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var document in solution.Projects.SelectMany(p => p.Documents))
        {
            if (document.FilePath == null || !workspace.Files.TryGetValue(document.FilePath, out var file))
                continue;
            var text = document.GetTextAsync().GetAwaiter().GetResult().ToString();
            if (text != file.Text)
                texts[document.FilePath] = text;
        }
        return texts;
    }
}
