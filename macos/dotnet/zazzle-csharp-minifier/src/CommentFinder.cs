// Finds the comments on a file and its non-local declarations, leaving comments inside bodies and initializers alone.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleMinifier;

// One comment to move: where it is in the source, what it describes, and its text.
internal sealed record FoundComment(TextSpan Span, SyntaxNode? Owner, string Raw, bool IsDoc);

// Walks a syntax tree and returns the comments that belong to the file or to a declaration.
internal static class CommentFinder
{
    // The comments to move, in source order.
    public static List<FoundComment> Find(SyntaxTree tree, SourceText text)
    {
        var root = tree.GetRoot();
        var fileHeader = FileHeaderComments(root);
        var found = new List<FoundComment>();
        foreach (var trivia in root.DescendantTrivia(descendIntoTrivia: false))
        {
            if (!IsComment(trivia) || IsKept(trivia))
                continue;
            if (fileHeader.Contains(trivia.FullSpan.Start))
            {
                found.Add(Make(trivia, null, text));
                continue;
            }
            if (IsLocal(trivia))
                continue;
            found.Add(Make(trivia, Owner(trivia), text));
        }
        return found;
    }

    // Builds the record, trimming the line break that ends a /// comment out of its span.
    private static FoundComment Make(SyntaxTrivia trivia, SyntaxNode? owner, SourceText text)
    {
        // A doc comment's Span leaves out its first /// marker; FullSpan covers it.
        var raw = text.ToString(trivia.FullSpan).TrimEnd('\r', '\n', ' ', '\t');
        var isDoc = trivia.IsKind(SyntaxKind.SingleLineDocumentationCommentTrivia) || trivia.IsKind(SyntaxKind.MultiLineDocumentationCommentTrivia);
        return new FoundComment(new TextSpan(trivia.FullSpan.Start, raw.Length), owner, raw, isDoc);
    }

    // True for //, /* */, /// and /** */ comments.
    private static bool IsComment(SyntaxTrivia trivia) =>
        trivia.Kind() is SyntaxKind.SingleLineCommentTrivia or SyntaxKind.MultiLineCommentTrivia
            or SyntaxKind.SingleLineDocumentationCommentTrivia or SyntaxKind.MultiLineDocumentationCommentTrivia;

    // True for comments that tools read as instructions (ReSharper switches, language= hints, this tool's unresolved-test marker), which must stay in the code.
    private static bool IsKept(SyntaxTrivia trivia)
    {
        var body = trivia.ToFullString().TrimStart('/', '*', ' ', '\t');
        return body.StartsWith("ReSharper ", StringComparison.Ordinal)
            || trivia.ToFullString().StartsWith(TestMethods.MarkerPrefix, StringComparison.Ordinal)
            || body.StartsWith("language=", StringComparison.Ordinal)
            || body.StartsWith("lang=", StringComparison.Ordinal);
    }

    // True when the comment sits inside a body, an initializer, a parameter list or a top-level statement.
    private static bool IsLocal(SyntaxTrivia trivia)
    {
        var pos = trivia.FullSpan.Start;
        foreach (var node in trivia.Token.Parent?.AncestorsAndSelf() ?? [])
        {
            if (node is GlobalStatementSyntax && node.FullSpan.Contains(pos))
                return true;
            var local = node is BlockSyntax or ArrowExpressionClauseSyntax or EqualsValueClauseSyntax
                or BaseParameterListSyntax or ConstructorInitializerSyntax;
            if (local && node.Span.Contains(pos))
                return true;
        }
        return false;
    }

    // The declaration the comment describes; a comment after an enum member's comma describes that member.
    private static SyntaxNode? Owner(SyntaxTrivia trivia)
    {
        var token = trivia.Token;
        if (token.IsKind(SyntaxKind.CommaToken) && token.Parent is EnumDeclarationSyntax e && token.TrailingTrivia.Contains(trivia))
        {
            var index = e.Members.GetSeparators().ToList().IndexOf(token);
            return e.Members[index];
        }
        return Declarations.OwnerOf(token.Parent);
    }

    // Starts of the comments at the top of the file that a blank line separates from the first declaration or statement, plus all comments above a using.
    private static HashSet<int> FileHeaderComments(SyntaxNode root)
    {
        var result = new HashSet<int>();
        var first = root.GetFirstToken(includeZeroWidth: true);
        var startsDeclaration = Declarations.OwnerOf(first.Parent) != null || first.Parent?.FirstAncestorOrSelf<GlobalStatementSyntax>() != null;
        var pending = new List<int>();
        var newlines = 0;
        foreach (var trivia in first.LeadingTrivia)
        {
            if (trivia.IsKind(SyntaxKind.EndOfLineTrivia) && ++newlines >= 2)
            {
                result.UnionWith(pending);
                pending.Clear();
            }
            if (!IsComment(trivia))
                continue;
            pending.Add(trivia.FullSpan.Start);
            // A /// comment carries its own line break.
            newlines = trivia.IsKind(SyntaxKind.SingleLineDocumentationCommentTrivia) ? 1 : 0;
        }
        if (!startsDeclaration)
            result.UnionWith(pending);
        return result;
    }
}
