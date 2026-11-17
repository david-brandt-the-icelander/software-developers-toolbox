// The --list-identifiers command: prints namespaces, types and members as an indented tree.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// Builds the identifier tree from the declarations in the source files.
internal static class Identifiers
{
    // Parse options: the newest language version, so new syntax does not read as errors.
    private static readonly CSharpParseOptions Options = CSharpParseOptions.Default.WithLanguageVersion(LanguageVersion.Preview);

    // The tree text for the files, generated code left out.
    public static string List(string root, IReadOnlyList<string> files) => IdentifierTree.Render(Build(root, files));

    // The tree of every non-local declaration in the files.
    public static IdentifierNode Build(string root, IReadOnlyList<string> files)
    {
        var tree = new IdentifierNode("");
        foreach (var path in files)
        {
            var file = SourceFile.Read(root, path, out _);
            if (file == null || SourceFiles.IsGenerated(path, file.Text))
                continue;
            foreach (var member in CSharpSyntaxTree.ParseText(file.Text, Options).GetCompilationUnitRoot().Members)
                Add(tree, member);
        }
        return tree;
    }

    // Adds a declaration and what it declares under the parent node.
    private static void Add(IdentifierNode parent, MemberDeclarationSyntax member)
    {
        if (member is BaseNamespaceDeclarationSyntax ns)
        {
            var node = ns.Name.ToString().Split('.').Select(p => p.Trim()).Aggregate(parent, (n, part) => n.Child(part));
            foreach (var inner in ns.Members)
                Add(node, inner);
            return;
        }
        foreach (var name in Names(member))
        {
            var node = parent.Child(name);
            if (member is TypeDeclarationSyntax type)
                foreach (var inner in type.Members)
                    Add(node, inner);
            if (member is EnumDeclarationSyntax e)
                foreach (var value in e.Members)
                    node.Child(value.Identifier.Text);
        }
    }

    // The names a declaration introduces: none for constructors, finalizers, operators, indexers and top-level statements.
    private static IEnumerable<string> Names(MemberDeclarationSyntax member) => member switch
    {
        BaseTypeDeclarationSyntax t => [t.Identifier.Text],
        DelegateDeclarationSyntax d => [d.Identifier.Text],
        MethodDeclarationSyntax m when m.ExplicitInterfaceSpecifier == null => [m.Identifier.Text],
        PropertyDeclarationSyntax p when p.ExplicitInterfaceSpecifier == null => [p.Identifier.Text],
        EventDeclarationSyntax e when e.ExplicitInterfaceSpecifier == null => [e.Identifier.Text],
        BaseFieldDeclarationSyntax f => f.Declaration.Variables.Select(v => v.Identifier.Text),
        _ => [],
    };
}
