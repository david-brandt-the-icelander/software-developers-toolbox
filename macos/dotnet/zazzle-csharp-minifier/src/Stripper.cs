// Removes comment spans from source text, tidies the lines they leave behind, and proves the code is unchanged.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleMinifier;

// One cut in the source: the range removed and what replaces it.
internal readonly record struct Cut(int Start, int End, string With, bool WholeLines);

// Cuts comments out of a file's text.
internal static class Stripper
{
    // The text with the spans removed: whole lines when a line held only comments, otherwise the comment and its padding.
    public static string Strip(SourceText text, IReadOnlyList<TextSpan> spans)
    {
        var masked = Mask(text.ToString(), spans);
        var cuts = Merge(spans.Select(s => CutFor(text, masked, s)));
        cuts = Merge(cuts.Select(c => c.WholeLines ? Widen(text, c) : c));
        var result = new System.Text.StringBuilder();
        var at = 0;
        foreach (var cut in cuts)
        {
            result.Append(text.ToString(TextSpan.FromBounds(at, cut.Start))).Append(cut.With);
            at = cut.End;
        }
        result.Append(text.ToString(TextSpan.FromBounds(at, text.Length)));
        return result.ToString();
    }

    // Null when the two texts have the same tokens and preprocessor directives, otherwise the first difference.
    public static string? CodeDifference(string before, string after, CSharpParseOptions options)
    {
        var a = Shape(CSharpSyntaxTree.ParseText(before, options));
        var b = Shape(CSharpSyntaxTree.ParseText(after, options));
        for (var i = 0; i < Math.Max(a.Count, b.Count); i++)
        {
            var x = i < a.Count ? a[i] : "<end>";
            var y = i < b.Count ? b[i] : "<end>";
            if (x != y)
                return $"'{x}' became '{y}'";
        }
        return null;
    }

    // The tokens, preprocessor directives and disabled code of a tree, in order, as text.
    private static List<string> Shape(SyntaxTree tree)
    {
        var shape = new List<string>();
        foreach (var token in tree.GetRoot().DescendantTokens())
        {
            foreach (var trivia in token.LeadingTrivia.Concat(token.TrailingTrivia))
                if (trivia.IsDirective || trivia.IsKind(SyntaxKind.DisabledTextTrivia))
                    shape.Add(trivia.ToString().Trim());
            shape.Add(token.Text);
        }
        return shape;
    }

    // The source with every span to cut blanked out, so the layout checks see past neighbouring comments.
    private static string Mask(string source, IReadOnlyList<TextSpan> spans)
    {
        var chars = source.ToCharArray();
        foreach (var span in spans)
            for (var i = span.Start; i < span.End; i++)
                if (chars[i] is not '\r' and not '\n')
                    chars[i] = ' ';
        return new string(chars);
    }

    // The cut for one comment, judged by what else stands on its first and last lines.
    private static Cut CutFor(SourceText text, string masked, TextSpan span)
    {
        var first = text.Lines.GetLineFromPosition(span.Start);
        var last = text.Lines.GetLineFromPosition(span.End);
        var before = masked[first.Start..span.Start];
        var after = masked[span.End..last.End];
        var spaceBefore = before.Length - before.TrimEnd().Length;
        var spaceAfter = after.Length - after.TrimStart().Length;
        if (before.Trim().Length == 0 && after.Trim().Length == 0)
            return new Cut(first.Start, last.EndIncludingLineBreak, "", true);
        if (after.Trim().Length == 0)
            return new Cut(span.Start - spaceBefore, span.End, "", false);
        var joins = spaceBefore == 0 && before.Trim().Length > 0 && spaceAfter == 0;
        return new Cut(span.Start, span.End + spaceAfter, joins ? " " : "", false);
    }

    // Also removes one blank line when a cut of whole lines would leave two together or a blank line hugging a brace or the file's start or end.
    private static Cut Widen(SourceText text, Cut cut)
    {
        var lines = text.Lines;
        var startLine = lines.GetLineFromPosition(cut.Start).LineNumber;
        var endLine = cut.End >= text.Length ? lines.Count : lines.GetLineFromPosition(cut.End).LineNumber;
        var prev = startLine > 0 ? text.ToString(lines[startLine - 1].Span).Trim() : null;
        var next = endLine < lines.Count && cut.End < text.Length ? text.ToString(lines[endLine].Span).Trim() : null;
        var prevOpens = prev == null || prev.Length == 0 || prev.EndsWith('{');
        if (prevOpens && next?.Length == 0)
            return cut with { End = lines[endLine].EndIncludingLineBreak };
        if (prev?.Length == 0 && (next == null || next.StartsWith('}')))
            return cut with { Start = lines[startLine - 1].Start };
        return cut;
    }

    // Sorts cuts and joins those that touch or overlap.
    private static List<Cut> Merge(IEnumerable<Cut> cuts)
    {
        var merged = new List<Cut>();
        foreach (var cut in cuts.OrderBy(c => c.Start))
        {
            if (merged.Count > 0 && cut.Start <= merged[^1].End)
            {
                var last = merged[^1];
                merged[^1] = last with { End = Math.Max(last.End, cut.End), With = last.With + cut.With, WholeLines = last.WholeLines && cut.WholeLines };
                continue;
            }
            merged.Add(cut);
        }
        return merged;
    }
}
