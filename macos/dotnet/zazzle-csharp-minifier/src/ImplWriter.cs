// Writes the implementation half of a split file: the same usings and namespaces, partial types, and the moved bodies.
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// Builds a new implementation file, or adds moved members to the one a previous split wrote.
internal static class ImplWriter
{
    // A namespace or type in the implementation file and what goes inside it, in source order.
    private sealed class Scope(SyntaxNode? decl)
    {
        public SyntaxNode? Decl { get; } = decl;
        public List<object> Items { get; } = [];
    }

    // The whole implementation file for the moved members of a file.
    public static string New(CompilationUnitSyntax root, string text, List<MovedMember> moved)
    {
        var nl = text.Contains("\r\n") ? "\r\n" : "\n";
        var top = new Scope(null);
        foreach (var m in moved)
            Place(top, m);
        var sb = new StringBuilder();
        foreach (var line in FileDirectives(root).Concat(Usings(root.Externs, root.Usings)))
            sb.Append(line).Append(nl);
        if (sb.Length > 0)
            sb.Append(nl);
        Render(top, text, nl, sb);
        return sb.ToString();
    }

    // The existing implementation file with the moved members added inside their types, or null with the type it lacks.
    public static string? Merge(CompilationUnitSyntax root, string existing, List<MovedMember> moved, out string? missing)
    {
        missing = null;
        var nl = existing.Contains("\r\n") ? "\r\n" : "\n";
        var target = (CompilationUnitSyntax)CSharpSyntaxTree.ParseText(existing).GetRoot();
        var types = target.DescendantNodes().OfType<TypeDeclarationSyntax>()
            .GroupBy(Declarations.QualifiedName, StringComparer.Ordinal).ToDictionary(g => g.Key, g => g.First(), StringComparer.Ordinal);
        var edits = new List<(int At, string With)>();
        foreach (var group in moved.GroupBy(m => Declarations.QualifiedName(m.Member.Parent!)))
        {
            if (!types.TryGetValue(group.Key, out var type))
            {
                missing = group.Key;
                return null;
            }
            var lineStart = existing.LastIndexOf('\n', type.CloseBraceToken.SpanStart - 1) + 1;
            edits.Add((lineStart, string.Concat(group.Select(m => nl + m.Implementation + nl))));
        }
        var have = target.Usings.Select(u => u.ToString()).ToHashSet(StringComparer.Ordinal);
        var add = Usings([], root.Usings).Where(u => !have.Contains(u)).ToList();
        if (add.Count > 0)
            edits.Add((target.Usings.Count > 0 ? target.Usings.Last().FullSpan.End : 0, string.Concat(add.Select(u => u + nl))));
        var result = existing;
        foreach (var (at, with) in edits.OrderByDescending(e => e.At))
            result = result[..at] + with + result[at..];
        return result;
    }

    // Adds the member under the chain of namespaces and types that holds it, creating scopes as needed.
    private static void Place(Scope top, MovedMember m)
    {
        var scope = top;
        foreach (var decl in m.Member.Ancestors().Where(a => a is BaseNamespaceDeclarationSyntax or TypeDeclarationSyntax).Reverse())
        {
            var next = scope.Items.OfType<Scope>().FirstOrDefault(s => s.Decl == decl);
            if (next == null)
            {
                next = new Scope(decl);
                scope.Items.Add(next);
            }
            scope = next;
        }
        scope.Items.Add(m);
    }

    // Writes a scope's items, one blank line between them.
    private static void Render(Scope scope, string text, string nl, StringBuilder sb)
    {
        for (var i = 0; i < scope.Items.Count; i++)
        {
            if (i > 0)
                sb.Append(nl);
            if (scope.Items[i] is MovedMember m)
                sb.Append(m.Implementation).Append(nl);
            else
                RenderScope((Scope)scope.Items[i], text, nl, sb);
        }
    }

    // Writes one namespace or partial type with its contents.
    private static void RenderScope(Scope scope, string text, string nl, StringBuilder sb)
    {
        var indent = HeaderSplitter.Indent(text, scope.Decl!.SpanStart);
        if (scope.Decl is FileScopedNamespaceDeclarationSyntax f)
        {
            sb.Append($"namespace {f.Name};").Append(nl).Append(nl);
            AppendUsings(f.Externs, f.Usings, indent, nl, sb);
            Render(scope, text, nl, sb);
            return;
        }
        sb.Append(indent).Append(Opening(scope.Decl)).Append(nl).Append(indent).Append('{').Append(nl);
        if (scope.Decl is NamespaceDeclarationSyntax n)
            AppendUsings(n.Externs, n.Usings, HeaderSplitter.Indent(text, n.OpenBraceToken.SpanStart) + "    ", nl, sb);
        Render(scope, text, nl, sb);
        sb.Append(indent).Append('}').Append(nl);
    }

    // The first line of a block namespace or a partial type: modifiers, partial, keyword, name and type parameters, no attributes or bases.
    private static string Opening(SyntaxNode decl)
    {
        if (decl is NamespaceDeclarationSyntax n)
            return $"namespace {n.Name}";
        var type = (TypeDeclarationSyntax)decl;
        var mods = type.Modifiers.Select(t => t.Text).ToList();
        if (!mods.Contains("partial"))
            mods.Add("partial");
        var keyword = type is RecordDeclarationSyntax r && r.ClassOrStructKeyword != default ? $"record {r.ClassOrStructKeyword.Text}" : HeaderSplitter.TypeKeyword(type).Text;
        return string.Join(" ", mods.Append(keyword)) + " " + type.Identifier.Text + type.TypeParameterList;
    }

    // Writes a namespace's own using directives, then a blank line.
    private static void AppendUsings(SyntaxList<ExternAliasDirectiveSyntax> externs, SyntaxList<UsingDirectiveSyntax> usings, string indent, string nl, StringBuilder sb)
    {
        var lines = Usings(externs, usings).ToList();
        foreach (var line in lines)
            sb.Append(indent).Append(line).Append(nl);
        if (lines.Count > 0)
            sb.Append(nl);
    }

    // Extern aliases and using directives as text, leaving out global usings, which already apply to every file.
    private static IEnumerable<string> Usings(SyntaxList<ExternAliasDirectiveSyntax> externs, SyntaxList<UsingDirectiveSyntax> usings) =>
        externs.Select(e => e.ToString()).Concat(usings.Where(u => u.GlobalKeyword == default).Select(u => u.ToString()));

    // The #nullable and #pragma warning directives outside any type, which also govern the moved bodies.
    private static IEnumerable<string> FileDirectives(CompilationUnitSyntax root) =>
        root.DescendantTrivia()
            .Where(t => t.Kind() is SyntaxKind.NullableDirectiveTrivia or SyntaxKind.PragmaWarningDirectiveTrivia)
            .Where(t => !HeaderSplitter.InsideType(root, t.SpanStart))
            .Select(t => t.ToString().Trim());
}
