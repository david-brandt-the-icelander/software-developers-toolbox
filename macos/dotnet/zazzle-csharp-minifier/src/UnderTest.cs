// Works out a test's class and method under test from what its // When section calls, using one compilation of the whole tree.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// Where a test belongs: the class under test (nested types joined by dots) and the member under test.
internal sealed record Target(string Class, string Method);

// A test's target, or the reason none was found.
internal sealed record Resolution(Target? Target, string? Reason);

// A compilation of every .cs file under the root, with the files that hold tests marked so their helpers are never the code under test.
internal sealed class UnderTest
{
    private readonly CSharpCompilation compilation;
    private readonly Dictionary<string, SyntaxTree> treesByPath;
    private readonly HashSet<SyntaxTree> testTrees;

    // Wraps a built compilation.
    private UnderTest(CSharpCompilation compilation, Dictionary<string, SyntaxTree> treesByPath, HashSet<SyntaxTree> testTrees)
    {
        this.compilation = compilation;
        this.treesByPath = treesByPath;
        this.testTrees = testTrees;
    }

    // Parses every readable .cs file under the root and compiles them against the .NET 8 reference assemblies.
    public static UnderTest Load(string root, CSharpParseOptions options)
    {
        var trees = new Dictionary<string, SyntaxTree>(StringComparer.Ordinal);
        foreach (var path in SourceFiles.Under([root]))
        {
            var file = SourceFile.Read(root, path, out _);
            if (file != null)
                trees[path] = CSharpSyntaxTree.ParseText(file.Text, options, path);
        }
        var tests = trees.Values.Where(t => TestMethods.In(t.GetRoot()).Count > 0).ToHashSet();
        var compilation = CSharpCompilation.Create("under-test", trees.Values, Basic.Reference.Assemblies.Net80.References.All,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, nullableContextOptions: NullableContextOptions.Enable));
        return new UnderTest(compilation, trees, tests);
    }

    // The compilation's tree for a file, or null when it could not be read.
    public SyntaxTree? Tree(string path) => treesByPath.GetValueOrDefault(path);

    // Resolves one test method declared in a tree of this compilation.
    public Resolution Resolve(MethodDeclarationSyntax test)
    {
        var body = (SyntaxNode?)test.Body ?? test.ExpressionBody ?? ImplementationBody(test);
        var section = body == null ? null : TestMethods.WhenSection(body);
        if (body == null || section == null)
            return new Resolution(null, "no // When section");
        var model = compilation.GetSemanticModel(body.SyntaxTree);
        var candidates = body.DescendantNodes()
            .Where(n => section.Value.Contains(n.Span) && IsCandidateKind(n))
            .Select(n => (Node: n, Symbol: Kept(model, n)))
            .Where(c => c.Symbol != null)
            .ToList();
        var outermost = candidates.Where(c => !candidates.Any(o => o.Node != c.Node && o.Node.Span.Contains(c.Node.Span))).ToList();
        var targets = outermost.Select(c => TargetOf(c.Symbol!)).Distinct().ToList();
        if (targets.Count == 0)
            return new Resolution(null, "nothing in // When calls non-test code");
        if (targets.Count > 1)
            return new Resolution(null, "// When calls several members: " + string.Join(", ", targets.Select(t => t.Class + "." + t.Method)));
        return new Resolution(targets[0], null);
    }

    // The body of the implementation part when the test is a partial method declared without one.
    private SyntaxNode? ImplementationBody(MethodDeclarationSyntax test)
    {
        var model = compilation.GetSemanticModel(test.SyntaxTree);
        var impl = model.GetDeclaredSymbol(test)?.PartialImplementationPart;
        var syntax = impl?.DeclaringSyntaxReferences.FirstOrDefault()?.GetSyntax() as MethodDeclarationSyntax;
        return (SyntaxNode?)syntax?.Body ?? syntax?.ExpressionBody;
    }

    // True for the expressions that can name a member under test: calls, constructions, member and element access, and operators.
    private static bool IsCandidateKind(SyntaxNode node) =>
        node is InvocationExpressionSyntax or BaseObjectCreationExpressionSyntax or MemberAccessExpressionSyntax
            or ElementAccessExpressionSyntax or BinaryExpressionSyntax or PrefixUnaryExpressionSyntax or PostfixUnaryExpressionSyntax;

    // The method or property the expression binds to, when it is declared in source outside the test files.
    private ISymbol? Kept(SemanticModel model, SyntaxNode node)
    {
        var info = model.GetSymbolInfo(node);
        var symbol = info.Symbol ?? SameMember(info.CandidateSymbols);
        symbol = symbol switch
        {
            IMethodSymbol { AssociatedSymbol: IPropertySymbol p } => p,
            IMethodSymbol m => m.ReducedFrom ?? m,
            _ => symbol,
        };
        if (symbol is not (IMethodSymbol or IPropertySymbol))
            return null;
        symbol = symbol.OriginalDefinition;
        var locations = symbol.Locations.Where(l => l.IsInSource).ToList();
        if (locations.Count == 0 || locations.Any(l => testTrees.Contains(l.SourceTree!)))
            return null;
        return symbol;
    }

    // The first candidate when overload resolution failed between members that share a name and type, otherwise null.
    private static ISymbol? SameMember(IReadOnlyList<ISymbol> candidates)
    {
        if (candidates.Count == 0)
            return null;
        var first = candidates[0];
        return candidates.All(c => c.Name == first.Name && SymbolEqualityComparer.Default.Equals(c.ContainingType, first.ContainingType)) ? first : null;
    }

    // The class and member names a symbol maps to.
    private static Target TargetOf(ISymbol symbol)
    {
        var types = new List<string>();
        for (var t = symbol.ContainingType; t != null; t = t.ContainingType)
            types.Insert(0, t.Name);
        var method = symbol switch
        {
            IMethodSymbol { MethodKind: MethodKind.Constructor or MethodKind.StaticConstructor } => "Constructor",
            IPropertySymbol { IsIndexer: true } => "Indexer",
            IMethodSymbol { MethodKind: MethodKind.UserDefinedOperator or MethodKind.Conversion } m => m.MetadataName,
            _ => symbol.Name,
        };
        return new Target(string.Join(".", types), method);
    }
}
