// The --strip-comments command: moves file and declaration comments out of C# files into Markdown under document/.
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Text;

namespace ZazzleMinifier;

// Plans, file by file, the Markdown to write and the stripped source, the Markdown first so no comment is lost.
internal static class StripComments
{
    // The folder under the root that mirrors the source tree.
    public const string DocFolder = "document";

    // Parse options: the newest language version, so new syntax does not read as errors.
    private static readonly CSharpParseOptions Options = CSharpParseOptions.Default.WithLanguageVersion(LanguageVersion.Preview);

    // The edits that move the comments out of every file.
    public static Plan Make(string root, IReadOnlyList<string> files)
    {
        var plan = new Plan();
        foreach (var path in files)
        {
            var file = SourceFile.Read(root, path, out var error);
            if (file == null)
            {
                plan.Errors.Add($"{Path.GetRelativePath(root, path)}: left unchanged: {error}");
                continue;
            }
            One(root, file, plan);
        }
        return plan;
    }

    // Adds one file's edits to the plan, or a note or error saying why it has none.
    private static void One(string root, SourceFile file, Plan plan)
    {
        if (SourceFiles.IsGenerated(file.Path, file.Text))
        {
            plan.Notes.Add($"{file.Rel}: skipped, generated code");
            return;
        }
        var source = SourceText.From(file.Text);
        var comments = CommentFinder.Find(CSharpSyntaxTree.ParseText(source, Options), source);
        if (comments.Count == 0)
            return;
        var stripped = Stripper.Strip(source, comments.Select(c => c.Span).ToList());
        var difference = Stripper.CodeDifference(file.Text, stripped, Options);
        if (difference != null)
        {
            plan.Errors.Add($"{file.Rel}: left unchanged: stripping would change the code: {difference}");
            return;
        }
        var docRel = $"{DocFolder}/{file.Rel}.md";
        var docPath = Path.Combine(root, docRel);
        plan.Write(docPath, DocText(docPath, file.Rel, CommentDoc.Sections(comments, source)));
        plan.Write(file.Path, stripped, file.Bom);
        plan.Changes.Add($"{file.Rel}: moved {comments.Count} comments to {docRel}");
    }

    // A new document with its title, or the existing one with the new sections appended.
    private static string DocText(string path, string rel, string sections)
    {
        if (!File.Exists(path))
            return CommentDoc.Title(rel) + sections;
        var existing = File.ReadAllText(path);
        return existing + (existing.EndsWith('\n') ? "" : "\n") + sections;
    }
}
