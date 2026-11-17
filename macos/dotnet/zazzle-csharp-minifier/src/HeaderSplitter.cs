// Finds the members whose bodies can move out of a file and rewrites the file as declarations only.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleMinifier;

// A member whose body moves: the text after its signature that leaves the header, and its implementing declaration.
internal sealed record MovedMember(MemberDeclarationSyntax Member, TextSpan Cut, string HeaderTail, string Implementation);

// Splits one parsed file into a header text and the members whose bodies move to the implementation file.
internal static class HeaderSplitter
{
    // The members to move, or null with a reason when the file must be left whole.
    public static List<MovedMember>? Find(CompilationUnitSyntax root, string text, LanguageVersion version, out string? skip)
    {
        skip = null;
        if (root.Members.Any(m => m is GlobalStatementSyntax))
        {
            skip = "top-level statements";
            return null;
        }
        var moved = root.DescendantNodes().OfType<MemberDeclarationSyntax>().Where(m => Splittable(m, version)).Select(m => Move(m, text)).ToList();
        if (moved.Count > 0 && BlockingDirective(root, moved) is { } directive)
        {
            skip = $"preprocessor directive {directive} inside a type";
            return null;
        }
        return moved;
    }

    // The header: each moved member's body replaced by a bodiless partial declaration, and every type around one made partial.
    public static string Header(string text, List<MovedMember> moved)
    {
        var edits = new List<(int At, int End, string With)>();
        foreach (var m in moved)
        {
            edits.Add((m.Cut.Start, m.Cut.End, m.HeaderTail));
            edits.Add((PartialAt(m.Member), PartialAt(m.Member), "partial "));
            if (!HasAccess(m.Member))
                edits.Add((AccessAt(m.Member), AccessAt(m.Member), "private "));
            var async = m.Member.Modifiers.FirstOrDefault(t => t.IsKind(SyntaxKind.AsyncKeyword));
            if (async != default)
                edits.Add((async.SpanStart, async.Span.End + async.TrailingTrivia.FullSpan.Length, ""));
        }
        foreach (var type in moved.SelectMany(m => m.Member.Ancestors().OfType<TypeDeclarationSyntax>()).Distinct())
            if (!type.Modifiers.Any(SyntaxKind.PartialKeyword))
                edits.Add((TypeKeyword(type).SpanStart, TypeKeyword(type).SpanStart, "partial "));
        var result = text;
        foreach (var (at, end, with) in edits.Select((e, i) => (e, i)).OrderByDescending(x => x.e.At).ThenByDescending(x => x.e.End).ThenBy(x => x.i).Select(x => x.e))
            result = result[..at] + with + result[end..];
        return result;
    }

    // True when the member's body can move out under this C# version.
    private static bool Splittable(MemberDeclarationSyntax m, LanguageVersion version)
    {
        if (!InClassOrStruct(m) || m.Modifiers.Any(t => t.Kind() is SyntaxKind.AbstractKeyword or SyntaxKind.ExternKeyword or SyntaxKind.PartialKeyword))
            return false;
        return m switch
        {
            MethodDeclarationSyntax x => version >= LanguageVersion.CSharp9 && (x.Body != null || x.ExpressionBody != null)
                && x.ExplicitInterfaceSpecifier == null && x.ParameterList.Parameters.All(p => p.Type != null),
            PropertyDeclarationSyntax p => version >= LanguageVersion.CSharp13 && p.ExplicitInterfaceSpecifier == null
                && p.Initializer == null && HasBodies(p.AccessorList, p.ExpressionBody),
            IndexerDeclarationSyntax i => version >= LanguageVersion.CSharp13 && i.ExplicitInterfaceSpecifier == null
                && HasBodies(i.AccessorList, i.ExpressionBody),
            ConstructorDeclarationSyntax c => version >= LanguageVersion.CSharp14 && !c.Modifiers.Any(SyntaxKind.StaticKeyword)
                && (c.Body != null || c.ExpressionBody != null),
            EventDeclarationSyntax e => version >= LanguageVersion.CSharp14 && e.ExplicitInterfaceSpecifier == null
                && HasBodies(e.AccessorList, null),
            _ => false,
        };
    }

    // True when every type around the member is a class, struct or record, never an interface.
    private static bool InClassOrStruct(MemberDeclarationSyntax m) =>
        m.Parent is TypeDeclarationSyntax && m.Ancestors().OfType<TypeDeclarationSyntax>().All(t => t is not InterfaceDeclarationSyntax);

    // True when the member has an expression body or accessors that all have bodies.
    private static bool HasBodies(AccessorListSyntax? accessors, ArrowExpressionClauseSyntax? arrow) =>
        arrow != null || (accessors != null && accessors.Accessors.Count > 0 && accessors.Accessors.All(a => a.Body != null || a.ExpressionBody != null));

    // The cut, header tail and implementing declaration for one member.
    private static MovedMember Move(MemberDeclarationSyntax m, string text)
    {
        var cutStart = m switch
        {
            MethodDeclarationSyntax x => x.ConstraintClauses.Count > 0 ? x.ConstraintClauses.Last().Span.End : x.ParameterList.Span.End,
            ConstructorDeclarationSyntax c => c.ParameterList.Span.End,
            PropertyDeclarationSyntax p => p.Identifier.Span.End,
            IndexerDeclarationSyntax i => i.ParameterList.Span.End,
            EventDeclarationSyntax e => e.Identifier.Span.End,
            _ => throw new InvalidOperationException(m.Kind().ToString()),
        };
        var cut = TextSpan.FromBounds(cutStart, m.Span.End);
        return new MovedMember(m, cut, HeaderTail(m), Indent(text, m.SpanStart) + Signature(m) + text[cut.Start..cut.End]);
    }

    // What replaces the body in the header: a semicolon, or an accessor list of bodiless accessors.
    private static string HeaderTail(MemberDeclarationSyntax m)
    {
        var (accessors, arrow) = m switch
        {
            PropertyDeclarationSyntax p => (p.AccessorList, p.ExpressionBody),
            IndexerDeclarationSyntax i => (i.AccessorList, i.ExpressionBody),
            _ => (null, null),
        };
        if (arrow != null)
            return " { get; }";
        if (accessors == null)
            return ";";
        return " { " + string.Join(" ", accessors.Accessors.Select(a => string.Join(" ", a.Modifiers.Select(t => t.Text).Append(a.Keyword.Text)) + ";")) + " }";
    }

    // The implementing declaration's signature: all modifiers, partial, and parameters without defaults or attributes.
    private static string Signature(MemberDeclarationSyntax m)
    {
        var mods = (HasAccess(m) ? "" : "private ") + string.Join("", m.Modifiers.Select(t => t.Text + " ")) + "partial ";
        return m switch
        {
            MethodDeclarationSyntax x => mods + x.ReturnType + " " + x.Identifier.Text + x.TypeParameterList + Params(x.ParameterList)
                + string.Concat(x.ConstraintClauses.Select(c => " " + c)),
            ConstructorDeclarationSyntax c => mods + c.Identifier.Text + Params(c.ParameterList),
            PropertyDeclarationSyntax p => mods + p.Type + " " + p.Identifier.Text,
            IndexerDeclarationSyntax i => mods + i.Type + " this" + Params(i.ParameterList),
            EventDeclarationSyntax e => mods + "event " + e.Type + " " + e.Identifier.Text,
            _ => throw new InvalidOperationException(m.Kind().ToString()),
        };
    }

    // The parameter list in its own brackets, keeping modifiers, types and names but no attributes or default values.
    private static string Params(BaseParameterListSyntax list)
    {
        var (open, close) = list is BracketedParameterListSyntax ? ("[", "]") : ("(", ")");
        var parts = list.Parameters.Select(p => string.Join(" ", p.Modifiers.Select(t => t.Text).Append(p.Type!.ToString()).Append(p.Identifier.Text)));
        return open + string.Join(", ", parts) + close;
    }

    // True when the member states its accessibility; a partial member without one must say private.
    private static bool HasAccess(MemberDeclarationSyntax m) =>
        m.Modifiers.Any(t => t.Kind() is SyntaxKind.PublicKeyword or SyntaxKind.PrivateKeyword or SyntaxKind.ProtectedKeyword or SyntaxKind.InternalKeyword);

    // Where an added "private" goes in the header: before the first modifier, or where "partial" goes when there is none.
    private static int AccessAt(MemberDeclarationSyntax m) => m.Modifiers.Count > 0 ? m.Modifiers[0].SpanStart : PartialAt(m);

    // Where "partial" goes in the header: right before the type, the event keyword or a constructor's name.
    private static int PartialAt(MemberDeclarationSyntax m) => m switch
    {
        MethodDeclarationSyntax x => x.ReturnType.SpanStart,
        ConstructorDeclarationSyntax c => c.Identifier.SpanStart,
        PropertyDeclarationSyntax p => p.Type.SpanStart,
        IndexerDeclarationSyntax i => i.Type.SpanStart,
        EventDeclarationSyntax e => e.EventKeyword.SpanStart,
        _ => throw new InvalidOperationException(m.Kind().ToString()),
    };

    // The keyword "partial" must precede: record for records, otherwise class or struct.
    public static SyntaxToken TypeKeyword(TypeDeclarationSyntax type) =>
        type is RecordDeclarationSyntax r ? r.Keyword : type.Keyword;

    // The spaces and tabs that start the line holding the position.
    public static string Indent(string text, int position)
    {
        var start = text.LastIndexOf('\n', Math.Max(0, position - 1)) + 1;
        var end = start;
        while (end < text.Length && text[end] is ' ' or '\t')
            end++;
        return text[start..end];
    }

    // True when the position lies within a type's braces or signature, not in the trivia before it.
    public static bool InsideType(SyntaxNode root, int position) =>
        root.DescendantNodes().OfType<BaseTypeDeclarationSyntax>().Any(t => t.Span.Contains(position));

    // The first directive that would be lost or misplaced by the split, or null when all are safe.
    private static string? BlockingDirective(CompilationUnitSyntax root, List<MovedMember> moved)
    {
        foreach (var trivia in root.DescendantTrivia())
        {
            if (!trivia.IsDirective || moved.Any(m => m.Cut.Contains(trivia.SpanStart)))
                continue;
            if (trivia.Kind() is SyntaxKind.RegionDirectiveTrivia or SyntaxKind.EndRegionDirectiveTrivia)
                continue;
            if (InsideType(root, trivia.SpanStart) || trivia.Kind() is not (SyntaxKind.NullableDirectiveTrivia or SyntaxKind.PragmaWarningDirectiveTrivia))
                return trivia.ToString().Trim();
        }
        return null;
    }
}
