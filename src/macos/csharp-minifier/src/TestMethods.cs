// Recognises test methods, their // When sections and the marker left on tests whose method under test is unresolved.
using System.Text.RegularExpressions;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Text;

namespace CSharpMinifier;

// Syntax-only helpers for test methods.
internal static partial class TestMethods
{
    // The marker's fixed text; grep for it to find tests the split could not place.
    public const string MarkerPrefix = "// csharp-minifier: unresolved method under test";

    // Attribute names that make a method a test, without the Attribute suffix.
    private static readonly HashSet<string> TestAttributes =
        ["Fact", "Theory", "Test", "TestMethod", "DataTestMethod", "TestCase", "TestCaseSource", "SkippableFact", "SkippableTheory"];

    // True when the method carries a test attribute from xUnit, NUnit or MSTest.
    public static bool IsTest(MethodDeclarationSyntax method) =>
        method.AttributeLists.SelectMany(l => l.Attributes).Any(a => TestAttributes.Contains(AttributeName(a)));

    // The test methods declared in a tree, in source order.
    public static List<MethodDeclarationSyntax> In(SyntaxNode root) =>
        root.DescendantNodes().OfType<MethodDeclarationSyntax>().Where(IsTest).ToList();

    // The attribute's last name part without its Attribute suffix.
    private static string AttributeName(AttributeSyntax attribute)
    {
        var name = attribute.Name switch
        {
            QualifiedNameSyntax q => q.Right.Identifier.Text,
            AliasQualifiedNameSyntax a => a.Name.Identifier.Text,
            SimpleNameSyntax s => s.Identifier.Text,
            _ => attribute.Name.ToString(),
        };
        return name.EndsWith("Attribute", StringComparison.Ordinal) ? name[..^"Attribute".Length] : name;
    }

    // The span between the first // When (or // Act) comment and the next // Then (or // Assert) comment or the body's end, or null without one.
    public static TextSpan? WhenSection(SyntaxNode body)
    {
        int? start = null;
        foreach (var trivia in body.DescendantTrivia())
        {
            if (!trivia.IsKind(SyntaxKind.SingleLineCommentTrivia))
                continue;
            var text = trivia.ToString();
            if (start == null && WhenComment().IsMatch(text))
                start = trivia.Span.End;
            else if (start != null && ThenComment().IsMatch(text))
                return TextSpan.FromBounds(start.Value, trivia.SpanStart);
        }
        return start == null ? null : TextSpan.FromBounds(start.Value, body.Span.End);
    }

    // The marker comment in the method, if it has one.
    public static SyntaxTrivia? Marker(MethodDeclarationSyntax method)
    {
        foreach (var trivia in method.DescendantTrivia())
            if (trivia.IsKind(SyntaxKind.SingleLineCommentTrivia) && trivia.ToString().StartsWith(MarkerPrefix, StringComparison.Ordinal))
                return trivia;
        return null;
    }

    // The full marker comment for a reason.
    public static string MarkerText(string reason) => $"{MarkerPrefix} ({reason})";

    // A line that opens the When section.
    [GeneratedRegex(@"^//\s*(when|act)\b", RegexOptions.IgnoreCase)]
    private static partial Regex WhenComment();

    // A line that closes the When section.
    [GeneratedRegex(@"^//\s*(then|assert)\b", RegexOptions.IgnoreCase)]
    private static partial Regex ThenComment();
}
