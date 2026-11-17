// Renders moved comments as the Markdown file that mirrors a source file under document/.
using System.Text;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleMinifier;

// Writes one Markdown section per file or declaration, in the order they first appear in the source.
internal static class CommentDoc
{
    // The title line of a new document for the source file at the given root-relative path.
    public static string Title(string relPath) => $"# {relPath}\n";

    // The sections for the comments, with "File" first when the file itself had comments.
    public static string Sections(IReadOnlyList<FoundComment> comments, SourceText text)
    {
        var groups = comments
            .GroupBy(c => c.Owner == null ? "## File" : $"## {Declarations.Kind(c.Owner)} `{Declarations.QualifiedName(c.Owner)}`")
            .OrderBy(g => g.First().Owner == null ? 0 : 1);
        var sb = new StringBuilder();
        foreach (var group in groups)
        {
            sb.Append('\n').Append(group.Key).Append("\n\n");
            sb.Append(Body(group.ToList(), text)).Append('\n');
        }
        return sb.ToString();
    }

    // The comments of one section: comments on adjacent lines stay one paragraph, others are split by a blank line.
    private static string Body(List<FoundComment> comments, SourceText text)
    {
        var sb = new StringBuilder();
        for (var i = 0; i < comments.Count; i++)
        {
            if (i > 0)
                sb.Append(Adjacent(comments[i - 1], comments[i], text) && !comments[i].IsDoc && !comments[i - 1].IsDoc ? "\n" : "\n\n");
            sb.Append(comments[i].IsDoc ? "```xml\n" + Plain(comments[i].Raw) + "\n```" : Escape(Plain(comments[i].Raw)));
        }
        return sb.ToString();
    }

    // True when the second comment starts on the line after the first one ends.
    private static bool Adjacent(FoundComment a, FoundComment b, SourceText text) =>
        text.Lines.GetLineFromPosition(b.Span.Start).LineNumber == text.Lines.GetLineFromPosition(a.Span.End).LineNumber + 1;

    // The comment's words without its //, ///, /* */ or leading * markers.
    public static string Plain(string raw)
    {
        var block = raw.StartsWith("/*", StringComparison.Ordinal);
        if (block)
        {
            raw = raw[2..];
            raw = (raw.EndsWith("*/", StringComparison.Ordinal) ? raw[..^2] : raw).TrimStart('*').TrimEnd('*');
        }
        var lines = raw.Split('\n').Select(l => l.TrimEnd('\r').TrimStart());
        lines = block ? lines.Select(l => l.StartsWith('*') ? l[1..] : l) : lines.Select(l => l.TrimStart('/'));
        var cleaned = lines.Select(l => l.StartsWith(' ') ? l[1..] : l).ToList();
        while (cleaned.Count > 0 && cleaned[0].Trim().Length == 0)
            cleaned.RemoveAt(0);
        while (cleaned.Count > 0 && cleaned[^1].Trim().Length == 0)
            cleaned.RemoveAt(cleaned.Count - 1);
        return string.Join("\n", cleaned.Select(l => l.TrimEnd()));
    }

    // Stops a comment line that starts with # from becoming a Markdown heading.
    private static string Escape(string text) =>
        string.Join("\n", text.Split('\n').Select(l => l.StartsWith('#') ? "\\" + l : l));
}
