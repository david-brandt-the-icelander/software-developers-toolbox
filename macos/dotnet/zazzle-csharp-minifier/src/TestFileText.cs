// Text surgery for the test split: cutting tests out, marking them, and writing or extending the files they move to.
using System.Text;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleMinifier;

// A test on its way to another file, with the namespace and type chain it must be declared in there.
internal sealed record MovedTest(SourceFile File, MethodDeclarationSyntax Method, string Text, string Indent, string? Namespace, bool FileScoped, List<TypeDeclarationSyntax> Chain)
{
    // The key that groups tests declared in the same type of the same namespace.
    public string Key => (Namespace ?? "") + "|" + string.Join(".", Chain.Select(t => t.Identifier.Text));
}

// Builds and edits test file text without disturbing the code it does not move.
internal static class TestFileText
{
    // The file's line break: CRLF when it uses one, otherwise LF.
    public static string NewLine(string text) => text.Contains("\r\n", StringComparison.Ordinal) ? "\r\n" : "\n";

    // The leading spaces and tabs of the line that holds the position.
    public static string Indent(SourceText text, int position)
    {
        var line = text.Lines.GetLineFromPosition(position);
        var s = text.ToString(line.Span);
        return s[..(s.Length - s.TrimStart(' ', '\t').Length)];
    }

    // The start of the method's first line that is not blank, so leading blank lines stay behind.
    public static int ContentStart(SourceText text, MethodDeclarationSyntax method)
    {
        var line = text.Lines.GetLineFromPosition(method.FullSpan.Start);
        while (line.End < method.SpanStart && text.ToString(line.Span).Trim().Length == 0)
            line = text.Lines[line.LineNumber + 1];
        return Math.Max(line.Start, method.FullSpan.Start);
    }

    // The method's text from its first non-blank line to its line break, ready to copy.
    public static string MethodText(SourceText text, MethodDeclarationSyntax method, string nl)
    {
        var s = text.ToString(TextSpan.FromBounds(ContentStart(text, method), method.FullSpan.End));
        return s.EndsWith('\n') ? s : s + nl;
    }

    // The cut that removes a method, taking one neighbouring blank line so none doubles up or hugs a brace.
    public static Cut RemovalCut(SourceText text, MethodDeclarationSyntax method)
    {
        var lines = text.Lines;
        var start = ContentStart(text, method);
        var end = method.FullSpan.End;
        var startLine = lines.GetLineFromPosition(start).LineNumber;
        var endLine = end >= text.Length ? lines.Count : lines.GetLineFromPosition(end).LineNumber;
        var prev = startLine > 0 ? text.ToString(lines[startLine - 1].Span).Trim() : null;
        var next = endLine < lines.Count && end < text.Length ? text.ToString(lines[endLine].Span).Trim() : null;
        if ((prev == null || prev.Length == 0 || prev.EndsWith('{')) && next?.Length == 0)
            end = lines[endLine].EndIncludingLineBreak;
        else if (prev?.Length == 0 && (next == null || next.StartsWith('}')))
            start = lines[startLine - 1].Start;
        return new Cut(start, end, "", true);
    }

    // The edit that adds or updates the unresolved marker as the body's first line, or null when it is already right.
    public static Cut? MarkerEdit(SourceText text, MethodDeclarationSyntax method, string reason, string nl)
    {
        var marker = TestMethods.MarkerText(reason);
        var existing = TestMethods.Marker(method);
        if (existing is { } e)
            return e.ToString() == marker ? null : new Cut(e.SpanStart, e.Span.End, marker, false);
        var methodIndent = Indent(text, method.SpanStart);
        if (method.Body == null)
            return new Cut(ContentStart(text, method), ContentStart(text, method), methodIndent + marker + nl, false);
        var open = method.Body.OpenBraceToken;
        var indent = method.Body.Statements.Count > 0 && Line(text, method.Body.Statements[0].SpanStart) > Line(text, open.SpanStart)
            ? Indent(text, method.Body.Statements[0].SpanStart)
            : methodIndent + "    ";
        if (open.TrailingTrivia.Any(t => t.IsKind(SyntaxKind.EndOfLineTrivia)))
            return new Cut(open.FullSpan.End, open.FullSpan.End, indent + marker + nl, false);
        return new Cut(open.Span.End, open.Span.End, nl + indent + marker + nl + indent, false);
    }

    // The cut that removes the line holding the marker.
    public static Cut? MarkerRemoval(SourceText text, MethodDeclarationSyntax method)
    {
        if (TestMethods.Marker(method) is not { } marker)
            return null;
        var line = text.Lines.GetLineFromPosition(marker.SpanStart);
        return new Cut(line.Start, line.EndIncludingLineBreak, "", true);
    }

    // The method's text with its marker line removed.
    public static string WithoutMarker(string methodText)
    {
        var lines = methodText.Split('\n');
        return string.Join("\n", lines.Where(l => !l.TrimStart().StartsWith(TestMethods.MarkerPrefix, StringComparison.Ordinal)));
    }

    // The edits that add "partial" to every type in the chain that lacks it.
    public static IEnumerable<Cut> PartialEdits(IEnumerable<TypeDeclarationSyntax> types) =>
        types.Where(t => !t.Modifiers.Any(SyntaxKind.PartialKeyword))
            .Select(t => new Cut(t.Keyword.SpanStart, t.Keyword.SpanStart, "partial ", false));

    // Applies cuts to the text, joining those that overlap.
    public static string Apply(string text, IEnumerable<Cut> cuts)
    {
        var merged = new List<Cut>();
        foreach (var cut in cuts.OrderBy(c => c.Start).ThenBy(c => c.End))
        {
            if (merged.Count > 0 && cut.Start < merged[^1].End)
            {
                var last = merged[^1];
                merged[^1] = last with { End = Math.Max(last.End, cut.End), With = last.With + cut.With };
                continue;
            }
            merged.Add(cut);
        }
        var sb = new StringBuilder();
        var at = 0;
        foreach (var cut in merged)
        {
            sb.Append(text, at, cut.Start - at).Append(cut.With);
            at = cut.End;
        }
        return sb.Append(text, at, text.Length - at).ToString();
    }

    // The zero-based line number of a position.
    private static int Line(SourceText text, int position) => text.Lines.GetLineFromPosition(position).LineNumber;

    // Re-indents a copied method without its marker, leaving lines inside multi-line string literals exactly as they were.
    public static string Reindent(MovedTest test, string newIndent)
    {
        if (test.Indent == newIndent)
            return WithoutMarker(test.Text);
        var text = test.Method.SyntaxTree.GetText();
        var firstLine = Line(text, ContentStart(text, test.Method));
        var frozen = FrozenLines(test.Method, text).Select(l => l - firstLine).ToHashSet();
        var lines = test.Text.Split('\n');
        for (var i = 0; i < lines.Length; i++)
            if (!frozen.Contains(i) && lines[i].StartsWith(test.Indent, StringComparison.Ordinal))
                lines[i] = newIndent + lines[i][test.Indent.Length..];
        return WithoutMarker(string.Join("\n", lines));
    }

    // Source line numbers that continue a string literal begun on an earlier line.
    private static IEnumerable<int> FrozenLines(SyntaxNode method, SourceText text)
    {
        var strings = method.DescendantNodes().Where(n => n is InterpolatedStringExpressionSyntax
            || n is LiteralExpressionSyntax l && l.Token.Value is string);
        foreach (var node in strings)
            for (var line = Line(text, node.SpanStart) + 1; line <= Line(text, node.Span.End); line++)
                yield return line;
    }

    // The declaration line of a type re-opened as partial: its modifiers, keyword, name and type parameters.
    private static string Header(TypeDeclarationSyntax type)
    {
        var modifiers = type.Modifiers.Where(m => !m.IsKind(SyntaxKind.PartialKeyword)).Select(m => m.Text).Append("partial");
        var keyword = type is RecordDeclarationSyntax r && !r.ClassOrStructKeyword.IsKind(SyntaxKind.None)
            ? "record " + r.ClassOrStructKeyword.Text
            : type.Keyword.Text;
        return string.Join(" ", modifiers) + " " + keyword + " " + type.Identifier.Text + (type.TypeParameterList?.ToString() ?? "");
    }

    // The chain of types, outermost first, written at a depth with the tests inside the innermost one.
    public static string TypeBlock(List<MovedTest> tests, int depth, string unit, string nl)
    {
        var chain = tests[0].Chain;
        var sb = new StringBuilder();
        for (var i = 0; i < chain.Count; i++)
        {
            var pad = string.Concat(Enumerable.Repeat(unit, depth + i));
            sb.Append(pad).Append(Header(chain[i])).Append(nl).Append(pad).Append('{').Append(nl);
        }
        var memberIndent = string.Concat(Enumerable.Repeat(unit, depth + chain.Count));
        sb.Append(string.Join(nl, tests.Select(t => Reindent(t, memberIndent))));
        for (var i = chain.Count - 1; i >= 0; i--)
            sb.Append(string.Concat(Enumerable.Repeat(unit, depth + i))).Append('}').Append(nl);
        return sb.ToString();
    }

    // The using directives and extern aliases at the top of a test's file, as written.
    public static IEnumerable<string> Usings(MovedTest test)
    {
        var unit = (CompilationUnitSyntax)test.Method.SyntaxTree.GetRoot();
        return unit.Externs.Select(e => e.ToString()).Concat(unit.Usings.Select(u => u.ToString()));
    }

    // A new file holding the tests, grouped by namespace and type, in the style of the first test's file.
    public static string NewFile(List<MovedTest> tests, string unit, string nl)
    {
        var sb = new StringBuilder();
        var usings = tests.SelectMany(Usings).Distinct().ToList();
        if (usings.Count > 0)
            sb.Append(string.Join(nl, usings)).Append(nl).Append(nl);
        var byNamespace = tests.GroupBy(t => t.Namespace).ToList();
        var fileScoped = byNamespace.Count == 1 && byNamespace[0].Key != null && tests[0].FileScoped;
        var blocks = new List<string>();
        foreach (var ns in byNamespace)
        {
            var types = ns.GroupBy(t => t.Key).Select(g => TypeBlock(g.ToList(), ns.Key == null || fileScoped ? 0 : 1, unit, nl));
            var body = string.Join(nl, types);
            if (ns.Key == null)
                blocks.Add(body);
            else if (fileScoped)
                blocks.Add($"namespace {ns.Key};{nl}{nl}{body}");
            else
                blocks.Add($"namespace {ns.Key}{nl}{{{nl}{body}}}{nl}");
        }
        return sb.Append(string.Join(nl, blocks)).ToString();
    }

    // The existing file with the tests added to their types (or new type blocks), and any missing usings, or null when they cannot be placed.
    public static string? Extend(string existing, List<MovedTest> tests, string unit, CSharpParseOptions options)
    {
        var nl = NewLine(existing);
        foreach (var group in tests.GroupBy(t => t.Key))
        {
            var next = AddGroup(existing, group.ToList(), unit, nl, options);
            if (next == null)
                return null;
            existing = next;
        }
        return AddUsings(existing, tests.SelectMany(Usings).Distinct(), nl, options);
    }

    // Adds one type's tests to the text: inside the matching type if there is one, otherwise as a new block.
    private static string? AddGroup(string existing, List<MovedTest> tests, string unit, string nl, CSharpParseOptions options)
    {
        var tree = CSharpSyntaxTree.ParseText(existing, options);
        var text = tree.GetText();
        var names = tests[0].Chain.Select(t => t.Identifier.Text).ToList();
        var match = tree.GetRoot().DescendantNodes().OfType<TypeDeclarationSyntax>()
            .FirstOrDefault(t => ChainNames(t).SequenceEqual(names) && NamespaceOf(t) == tests[0].Namespace);
        if (match != null)
        {
            var close = match.CloseBraceToken;
            if (Indent(text, close.SpanStart).Length != close.SpanStart - text.Lines.GetLineFromPosition(close.SpanStart).Start)
                return null;
            var memberIndent = Indent(text, close.SpanStart) + unit;
            var add = (match.Members.Count > 0 ? nl : "") + string.Join(nl, tests.Select(t => Reindent(t, memberIndent)));
            var at = text.Lines.GetLineFromPosition(close.SpanStart).Start;
            return existing[..at] + add + existing[at..];
        }
        return AddBlock(existing, tree, tests, unit, nl);
    }

    // Appends a new type block in the matching namespace, or null when the file's namespace layout cannot take it.
    private static string? AddBlock(string existing, SyntaxTree tree, List<MovedTest> tests, string unit, string nl)
    {
        var root = (CompilationUnitSyntax)tree.GetRoot();
        var ns = tests[0].Namespace;
        var fileScoped = root.Members.OfType<FileScopedNamespaceDeclarationSyntax>().FirstOrDefault();
        var sep = existing.EndsWith('\n') ? nl : nl + nl;
        if (fileScoped != null)
            return fileScoped.Name.ToString() == ns ? existing + sep + TypeBlock(tests, 0, unit, nl) : null;
        if (ns == null)
            return existing + sep + TypeBlock(tests, 0, unit, nl);
        var block = root.Members.OfType<NamespaceDeclarationSyntax>().FirstOrDefault(n => n.Name.ToString() == ns);
        if (block == null)
            return existing + sep + $"namespace {ns}{nl}{{{nl}{TypeBlock(tests, 1, unit, nl)}}}{nl}";
        var at = tree.GetText().Lines.GetLineFromPosition(block.CloseBraceToken.SpanStart).Start;
        return existing[..at] + nl + TypeBlock(tests, 1, unit, nl) + existing[at..];
    }

    // Inserts the usings the file lacks after its last using, or at its top.
    private static string AddUsings(string existing, IEnumerable<string> usings, string nl, CSharpParseOptions options)
    {
        var root = (CompilationUnitSyntax)CSharpSyntaxTree.ParseText(existing, options).GetRoot();
        var have = root.Externs.Select(e => e.ToString()).Concat(root.Usings.Select(u => u.ToString())).ToHashSet();
        var missing = usings.Where(u => !have.Contains(u)).ToList();
        if (missing.Count == 0)
            return existing;
        var add = string.Join("", missing.Select(u => u + nl));
        if (root.Usings.Count == 0)
            return add + nl + existing;
        var at = root.Usings[^1].FullSpan.End;
        return existing[..at] + add + existing[at..];
    }

    // The names of a type and the types around it, outermost first.
    public static List<string> ChainNames(TypeDeclarationSyntax type) =>
        type.AncestorsAndSelf().OfType<TypeDeclarationSyntax>().Reverse().Select(t => t.Identifier.Text).ToList();

    // The dotted name of the namespaces around a node, or null in the global namespace.
    public static string? NamespaceOf(SyntaxNode node)
    {
        var names = node.Ancestors().OfType<BaseNamespaceDeclarationSyntax>().Reverse().Select(n => n.Name.ToString()).ToList();
        return names.Count == 0 ? null : string.Join(".", names);
    }

    // True when nothing in the file is worth keeping: only bare partial types with no members, attributes, bases or constraints.
    public static bool IsEmpty(SyntaxNode root)
    {
        var unit = (CompilationUnitSyntax)root;
        return unit.AttributeLists.Count == 0 && unit.Members.All(EmptyMember);
    }

    // True for a namespace of empty members, or a type that adds nothing beyond its name.
    private static bool EmptyMember(MemberDeclarationSyntax member) => member switch
    {
        BaseNamespaceDeclarationSyntax ns => ns.Members.All(EmptyMember),
        TypeDeclarationSyntax t => t.AttributeLists.Count == 0 && t.BaseList == null && t.ConstraintClauses.Count == 0
            && t.Modifiers.All(m => m.Kind() is SyntaxKind.PartialKeyword or SyntaxKind.PublicKeyword or SyntaxKind.InternalKeyword
                or SyntaxKind.PrivateKeyword or SyntaxKind.ProtectedKeyword)
            && t.Members.All(EmptyMember),
        _ => false,
    };
}
