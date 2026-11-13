// C# symbol and identifier extraction with Roslyn's syntax trees (no compilation, so it is fast and exact).
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleIndexer.Analysis;

// Static extractor.
public static class CSharpSymbols
{
    private const int MaxLinesPerName = 200;

    // Parses source text and returns its declared symbols and identifier occurrences.
    public static (List<Sym> syms, Dictionary<string, List<int>> idents) Extract(string source)
    {
        var tree = CSharpSyntaxTree.ParseText(source);
        var text = tree.GetText();
        var root = tree.GetRoot();
        var syms = new List<Sym>();
        foreach (var node in root.DescendantNodes())
            if (Declare(node) is { } d)
                syms.Add(new Sym(d.name, d.kind, Container(node), Line(text, d.token), text.Lines.GetLinePosition(node.Span.End).Line + 1, text.Lines[text.Lines.GetLinePosition(d.token.SpanStart).Line].ToString().Trim() is { Length: > 140 } l ? l[..140] : text.Lines[text.Lines.GetLinePosition(d.token.SpanStart).Line].ToString().Trim()));
        var idents = new Dictionary<string, List<int>>();
        foreach (var t in root.DescendantTokens().Where(t => t.IsKind(SyntaxKind.IdentifierToken)))
        {
            if (!idents.TryGetValue(t.ValueText, out var lines))
                idents[t.ValueText] = lines = [];
            if (lines.Count < MaxLinesPerName)
                lines.Add(IsDeclaration(t) ? -Line(text, t) : Line(text, t));
        }
        return (syms, idents);
    }

    // Returns the 1-based line of a token.
    private static int Line(Microsoft.CodeAnalysis.Text.SourceText text, SyntaxToken t) => text.Lines.GetLinePosition(t.SpanStart).Line + 1;

    // Maps a declaration node to its name token and kind, or null for other nodes.
    private static (string name, string kind, SyntaxToken token)? Declare(SyntaxNode n) => n switch
    {
        BaseNamespaceDeclarationSyntax x => (x.Name.ToString(), "namespace", x.Name.GetFirstToken()),
        ClassDeclarationSyntax x => (x.Identifier.Text, "class", x.Identifier),
        StructDeclarationSyntax x => (x.Identifier.Text, "struct", x.Identifier),
        InterfaceDeclarationSyntax x => (x.Identifier.Text, "interface", x.Identifier),
        RecordDeclarationSyntax x => (x.Identifier.Text, "record", x.Identifier),
        EnumDeclarationSyntax x => (x.Identifier.Text, "enum", x.Identifier),
        EnumMemberDeclarationSyntax x => (x.Identifier.Text, "enummember", x.Identifier),
        DelegateDeclarationSyntax x => (x.Identifier.Text, "delegate", x.Identifier),
        MethodDeclarationSyntax x => (x.Identifier.Text, "method", x.Identifier),
        ConstructorDeclarationSyntax x => (x.Identifier.Text, "constructor", x.Identifier),
        PropertyDeclarationSyntax x => (x.Identifier.Text, "property", x.Identifier),
        EventDeclarationSyntax x => (x.Identifier.Text, "event", x.Identifier),
        IndexerDeclarationSyntax x => ("this[]", "indexer", x.ThisKeyword),
        VariableDeclaratorSyntax { Parent.Parent: FieldDeclarationSyntax } x => (x.Identifier.Text, "field", x.Identifier),
        VariableDeclaratorSyntax { Parent.Parent: EventFieldDeclarationSyntax } x => (x.Identifier.Text, "event", x.Identifier),
        _ => null,
    };

    // Reports whether an identifier token is the name being declared by its parent.
    private static bool IsDeclaration(SyntaxToken t) => t.Parent switch
    {
        BaseTypeDeclarationSyntax x => x.Identifier == t,
        MethodDeclarationSyntax x => x.Identifier == t,
        ConstructorDeclarationSyntax x => x.Identifier == t,
        PropertyDeclarationSyntax x => x.Identifier == t,
        EventDeclarationSyntax x => x.Identifier == t,
        EnumMemberDeclarationSyntax x => x.Identifier == t,
        DelegateDeclarationSyntax x => x.Identifier == t,
        VariableDeclaratorSyntax x => x.Identifier == t,
        ParameterSyntax x => x.Identifier == t,
        _ => false,
    };

    // Builds the dotted container name from enclosing namespaces and types.
    private static string Container(SyntaxNode n) =>
        string.Join('.', n.Ancestors().Select(a => a switch
        {
            BaseNamespaceDeclarationSyntax x => x.Name.ToString(),
            BaseTypeDeclarationSyntax x => x.Identifier.Text,
            _ => null,
        }).Where(x => x is not null).Reverse());
}
