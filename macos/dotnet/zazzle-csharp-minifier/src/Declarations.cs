// Names and kinds of the non-local declarations that comments are moved from.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// Decides which syntax nodes own comments and how to name them in the docs.
internal static class Declarations
{
    // True for namespaces, types, members, enum members and accessors; false for top-level statements and broken members.
    public static bool IsOwner(SyntaxNode node) =>
        node is AccessorDeclarationSyntax
        || node is MemberDeclarationSyntax and not GlobalStatementSyntax and not IncompleteMemberSyntax;

    // The innermost owner that contains the node, or null when the node belongs to the file itself.
    public static SyntaxNode? OwnerOf(SyntaxNode? node) => node?.AncestorsAndSelf().FirstOrDefault(IsOwner);

    // The kind word shown in the docs, such as "class" or "method".
    public static string Kind(SyntaxNode node) => node switch
    {
        BaseNamespaceDeclarationSyntax => "namespace",
        RecordDeclarationSyntax r => r.ClassOrStructKeyword.IsKind(SyntaxKind.StructKeyword) ? "record struct" : "record",
        ClassDeclarationSyntax => "class",
        StructDeclarationSyntax => "struct",
        InterfaceDeclarationSyntax => "interface",
        EnumDeclarationSyntax => "enum",
        DelegateDeclarationSyntax => "delegate",
        MethodDeclarationSyntax => "method",
        ConstructorDeclarationSyntax => "constructor",
        DestructorDeclarationSyntax => "finalizer",
        OperatorDeclarationSyntax => "operator",
        ConversionOperatorDeclarationSyntax => "conversion operator",
        PropertyDeclarationSyntax => "property",
        IndexerDeclarationSyntax => "indexer",
        EventDeclarationSyntax or EventFieldDeclarationSyntax => "event",
        FieldDeclarationSyntax f => f.Modifiers.Any(SyntaxKind.ConstKeyword) ? "constant" : "field",
        EnumMemberDeclarationSyntax => "enum member",
        AccessorDeclarationSyntax => "accessor",
        _ => "declaration",
    };

    // The dotted name from the outermost namespace down to the node, such as "App.Store.Add(string, int)".
    public static string QualifiedName(SyntaxNode node) =>
        string.Join(".", node.AncestorsAndSelf().Where(IsOwner).Reverse().Select(Name));

    // The node's own name, with type parameters and parameter types where they tell overloads apart.
    private static string Name(SyntaxNode node) => node switch
    {
        BaseNamespaceDeclarationSyntax n => n.Name.ToString(),
        TypeDeclarationSyntax t => t.Identifier.Text + TypeParams(t.TypeParameterList),
        EnumDeclarationSyntax e => e.Identifier.Text,
        DelegateDeclarationSyntax d => d.Identifier.Text + TypeParams(d.TypeParameterList) + Params(d.ParameterList),
        MethodDeclarationSyntax m => Explicit(m.ExplicitInterfaceSpecifier) + m.Identifier.Text + TypeParams(m.TypeParameterList) + Params(m.ParameterList),
        ConstructorDeclarationSyntax c => c.Identifier.Text + Params(c.ParameterList),
        DestructorDeclarationSyntax d => "~" + d.Identifier.Text + "()",
        OperatorDeclarationSyntax o => "operator " + o.OperatorToken.Text + Params(o.ParameterList),
        ConversionOperatorDeclarationSyntax c => c.ImplicitOrExplicitKeyword.Text + " operator " + c.Type + Params(c.ParameterList),
        PropertyDeclarationSyntax p => Explicit(p.ExplicitInterfaceSpecifier) + p.Identifier.Text,
        IndexerDeclarationSyntax i => Explicit(i.ExplicitInterfaceSpecifier) + "this" + Params(i.ParameterList),
        EventDeclarationSyntax e => Explicit(e.ExplicitInterfaceSpecifier) + e.Identifier.Text,
        BaseFieldDeclarationSyntax f => string.Join(", ", f.Declaration.Variables.Select(v => v.Identifier.Text)),
        EnumMemberDeclarationSyntax m => m.Identifier.Text,
        AccessorDeclarationSyntax a => a.Keyword.Text,
        _ => node.Kind().ToString(),
    };

    // "IFoo." for an explicit interface implementation, otherwise nothing.
    private static string Explicit(ExplicitInterfaceSpecifierSyntax? spec) => spec == null ? "" : spec.Name + ".";

    // "<T, U>" for a generic declaration, otherwise nothing.
    private static string TypeParams(TypeParameterListSyntax? list) =>
        list == null ? "" : "<" + string.Join(", ", list.Parameters.Select(p => p.Identifier.Text)) + ">";

    // The parameter types in the declaration's own brackets, with ref/out/in/params kept.
    private static string Params(BaseParameterListSyntax list)
    {
        var (open, close) = list is BracketedParameterListSyntax ? ("[", "]") : ("(", ")");
        var parts = list.Parameters.Select(p => string.Join(" ", p.Modifiers.Select(m => m.Text).Append(p.Type?.ToString() ?? p.Identifier.Text)));
        return open + string.Join(", ", parts) + close;
    }
}
