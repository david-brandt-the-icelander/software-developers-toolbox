// The --split-headers command: splits each .cs file into declarations that stay and bodies that move to src/<name>.cs beside it.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// Plans the header/implementation split file by file, using the C# version of each file's project.
internal static class SplitHeaders
{
    // The global usings that SDK projects with ImplicitUsings add, so the compile check sees what the project sees.
    private const string ImplicitUsings = "global using System; global using System.Collections.Generic; global using System.IO; global using System.Linq; global using System.Net.Http; global using System.Threading; global using System.Threading.Tasks;";

    // The edits that split the files.
    public static Plan Make(string root, IReadOnlyList<string> files)
    {
        var plan = new Plan();
        foreach (var path in files)
        {
            var file = SourceFile.Read(root, path, out var error);
            if (file == null)
                plan.Errors.Add($"{Path.GetRelativePath(root, path).Replace('\\', '/')}: left unchanged: {error}");
            else
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
        var (version, why) = ProjectLanguage.For(root, file.Path);
        if (version == null || version < LanguageVersion.CSharp9)
        {
            plan.Notes.Add($"{file.Rel}: skipped, {why ?? "C# 9 or later needed"}");
            return;
        }
        var options = CSharpParseOptions.Default.WithLanguageVersion(version.Value);
        var tree = CSharpSyntaxTree.ParseText(file.Text, options);
        if (tree.GetDiagnostics().Any(d => d.Severity == DiagnosticSeverity.Error))
        {
            plan.Notes.Add($"{file.Rel}: skipped, it does not parse");
            return;
        }
        var root2 = (CompilationUnitSyntax)tree.GetRoot();
        var moved = HeaderSplitter.Find(root2, file.Text, version.Value, out var skip);
        if (skip != null)
            plan.Notes.Add($"{file.Rel}: skipped, {skip}");
        if (moved == null || moved.Count == 0)
            return;
        Split(root, file, root2, moved, options, plan);
    }

    // Builds and checks the two halves, then queues the implementation before the header so no body is ever lost.
    private static void Split(string root, SourceFile file, CompilationUnitSyntax syntax, List<MovedMember> moved, CSharpParseOptions options, Plan plan)
    {
        var implPath = Layout.ImplPath(root, file.Path);
        var implRel = Path.GetRelativePath(root, implPath).Replace('\\', '/');
        var existing = File.Exists(implPath) ? SourceFile.Read("/", implPath, out _) : null;
        if (File.Exists(implPath) && existing == null)
        {
            plan.Errors.Add($"{file.Rel}: left unchanged: {implRel} is not UTF-8");
            return;
        }
        string? missing = null;
        var impl = existing == null ? ImplWriter.New(syntax, file.Text, moved) : ImplWriter.Merge(syntax, existing.Text, moved, out missing);
        if (impl == null)
        {
            plan.Errors.Add($"{file.Rel}: left unchanged: {implRel} exists but has no partial {missing} to add the bodies to");
            return;
        }
        var header = HeaderSplitter.Header(file.Text, moved);
        var problem = Problem([file.Text, .. existing == null ? Array.Empty<string>() : [existing.Text]], [header, impl], options);
        if (problem != null)
        {
            plan.Errors.Add($"{file.Rel}: left unchanged: the split would not compile: {problem}");
            return;
        }
        plan.Write(implPath, impl, existing?.Bom ?? file.Bom);
        plan.Write(file.Path, header, file.Bom);
        plan.Changes.Add($"{file.Rel}: moved {moved.Count} bodies to {implRel}");
    }

    // The first syntax error in the halves, or the first compile error the split adds; null when the split is safe.
    private static string? Problem(string[] before, string[] after, CSharpParseOptions options)
    {
        var syntax = after.SelectMany(t => CSharpSyntaxTree.ParseText(t, options).GetDiagnostics()).FirstOrDefault(d => d.Severity == DiagnosticSeverity.Error);
        if (syntax != null)
            return syntax.ToString();
        var old = Errors(before, options);
        return Errors(after, options).FirstOrDefault(e => !old.Contains(e));
    }

    // The distinct compile errors of the texts compiled alone against the .NET 8 reference assemblies.
    private static HashSet<string> Errors(string[] texts, CSharpParseOptions options)
    {
        var trees = texts.Append(ImplicitUsings).Select(t => CSharpSyntaxTree.ParseText(t, options));
        var compilation = CSharpCompilation.Create("split-check", trees, Basic.Reference.Assemblies.Net80.References.All,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, allowUnsafe: true, nullableContextOptions: NullableContextOptions.Enable));
        return compilation.GetDiagnostics().Where(d => d.Severity == DiagnosticSeverity.Error).Select(d => d.Id + " " + d.GetMessage()).ToHashSet();
    }
}
