// The --split-tests command: moves each test into <ClassUnderTest>/<MethodUnderTest>.cs and marks the tests it cannot place.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// Plans the test-file split for the whole set of files at once, so tests from several files can share a target.
internal static class SplitTests
{
    // Parse options: the newest language version, so new syntax does not read as errors.
    private static readonly CSharpParseOptions Options = CSharpParseOptions.Default.WithLanguageVersion(LanguageVersion.Preview);

    // One file's final text and whether it keeps a byte-order mark.
    private sealed record Output(string Text, bool Bom);

    // What the split did to one source file, for its report line.
    private sealed class Report
    {
        public List<string> Targets { get; } = [];
        public int Moved { get; set; }
        public int Marked { get; set; }
        public int Cleared { get; set; }
    }

    // The edits that split the test files, or none (with an error) when any part fails its safety check.
    public static Plan Make(string root, IReadOnlyList<string> files)
    {
        var plan = new Plan();
        var sources = TestSources(root, files, plan);
        if (sources.Count == 0)
            return plan;
        var workspace = UnderTest.Load(root, Options);
        var outputs = new Dictionary<string, Output>(StringComparer.Ordinal);
        var moving = new Dictionary<string, List<MovedTest>>(StringComparer.Ordinal);
        var reports = new Dictionary<SourceFile, Report>();
        foreach (var source in sources)
        {
            var tree = workspace.Tree(source.Path);
            if (tree == null)
            {
                plan.Notes.Add($"{source.Rel}: skipped, outside the folders that are compiled");
                continue;
            }
            Split(source, tree, workspace, outputs, moving, reports);
        }
        var bases = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var (dest, tests) in moving)
            if (!Place(root, dest, tests, outputs, bases, plan))
                return Failed(plan);
        foreach (var source in sources.Where(s => outputs.ContainsKey(s.Path)))
            bases.TryAdd(source.Path, source.Text);
        if (!Verify(bases.Values.ToList(), outputs, moving.Keys, plan))
            return Failed(plan);
        Emit(root, outputs, moving.Keys.ToHashSet(StringComparer.Ordinal), reports, plan);
        return plan;
    }

    // The readable, non-generated files that declare tests, noting the generated ones.
    private static List<SourceFile> TestSources(string root, IReadOnlyList<string> files, Plan plan)
    {
        var sources = new List<SourceFile>();
        foreach (var path in files)
        {
            var file = SourceFile.Read(root, path, out var error);
            if (file == null)
            {
                plan.Errors.Add($"{Path.GetRelativePath(root, path)}: left unchanged: {error}");
                continue;
            }
            if (TestMethods.In(CSharpSyntaxTree.ParseText(file.Text, Options).GetRoot()).Count == 0)
                continue;
            if (SourceFiles.IsGenerated(file.Path, file.Text))
            {
                plan.Notes.Add($"{file.Rel}: skipped, generated code");
                continue;
            }
            sources.Add(file);
        }
        return sources;
    }

    // Decides every test in one file: queue it for its target, leave it, or mark it; then records the file's new text.
    private static void Split(SourceFile source, SyntaxTree tree, UnderTest workspace,
        Dictionary<string, Output> outputs, Dictionary<string, List<MovedTest>> moving, Dictionary<SourceFile, Report> reports)
    {
        var text = tree.GetText();
        var nl = TestFileText.NewLine(source.Text);
        var cuts = new List<Cut>();
        var types = new HashSet<TypeDeclarationSyntax>();
        var report = new Report();
        foreach (var test in TestMethods.In(tree.GetRoot()))
        {
            var resolution = workspace.Resolve(test);
            var chain = test.Ancestors().OfType<TypeDeclarationSyntax>().Reverse().ToList();
            if (resolution.Target == null || chain.Count == 0)
            {
                // A declaration whose body --split-headers moved keeps its marker with the body, in the src/ half.
                if (test.Body == null && test.ExpressionBody == null)
                    continue;
                var edit = TestFileText.MarkerEdit(text, test, resolution.Reason ?? "not inside a type", nl);
                if (edit != null)
                {
                    cuts.Add(edit.Value);
                    report.Marked++;
                }
                continue;
            }
            var dest = Destination(source.Path, resolution.Target);
            if (dest == source.Path)
            {
                if (TestFileText.MarkerRemoval(text, test) is { } clear)
                {
                    cuts.Add(clear);
                    report.Cleared++;
                }
                continue;
            }
            var fileScoped = test.Ancestors().OfType<FileScopedNamespaceDeclarationSyntax>().Any();
            var moved = new MovedTest(source, test, TestFileText.MethodText(text, test, nl), TestFileText.Indent(text, test.SpanStart),
                TestFileText.NamespaceOf(test), fileScoped, chain);
            if (!moving.TryGetValue(dest, out var list))
                moving[dest] = list = [];
            list.Add(moved);
            cuts.Add(TestFileText.RemovalCut(text, test));
            types.UnionWith(chain);
            report.Moved++;
            if (!report.Targets.Contains(dest))
                report.Targets.Add(dest);
        }
        if (cuts.Count == 0)
            return;
        cuts.AddRange(TestFileText.PartialEdits(types));
        outputs[source.Path] = new Output(TestFileText.Apply(source.Text, cuts), source.Bom);
        reports[source] = report;
    }

    // <base>/<Class>/<Method>.cs, where base is the file's folder, or its parent when the file already sits in a folder named after the class.
    private static string Destination(string file, Target target)
    {
        var dir = Path.GetDirectoryName(file)!;
        var baseDir = Path.GetFileName(dir) == target.Class ? Path.GetDirectoryName(dir)! : dir;
        return Path.Combine(baseDir, target.Class, target.Method + ".cs");
    }

    // Writes the target file's text from scratch or by extending its current text; false (with an error) when it cannot be extended.
    private static bool Place(string root, string dest, List<MovedTest> tests,
        Dictionary<string, Output> outputs, Dictionary<string, string> bases, Plan plan)
    {
        var unit = Unit(tests[0]);
        Output? current = outputs.GetValueOrDefault(dest);
        if (current == null && File.Exists(dest))
        {
            var disk = SourceFile.Read(root, dest, out var error);
            if (disk == null)
            {
                plan.Errors.Add($"{Path.GetRelativePath(root, dest)}: cannot add tests: {error}");
                return false;
            }
            bases[dest] = disk.Text;
            current = new Output(disk.Text, disk.Bom);
        }
        if (current == null)
        {
            outputs[dest] = new Output(TestFileText.NewFile(tests, unit, TestFileText.NewLine(tests[0].File.Text)), tests[0].File.Bom);
            return true;
        }
        var extended = TestFileText.Extend(current.Text, tests, unit, Options);
        if (extended == null)
        {
            plan.Errors.Add($"{Path.GetRelativePath(root, dest)}: cannot add tests: its namespaces or braces do not fit");
            return false;
        }
        outputs[dest] = current with { Text = extended };
        return true;
    }

    // The source's indent step, worked out from the test's indent and how deeply it is nested; four spaces when unclear.
    private static string Unit(MovedTest test)
    {
        var depth = test.Chain.Count + (test.FileScoped ? 0 : test.Method.Ancestors().OfType<NamespaceDeclarationSyntax>().Count());
        if (depth == 0 || test.Indent.Length == 0 || test.Indent.Length % depth != 0)
            return "    ";
        return test.Indent[..(test.Indent.Length / depth)];
    }

    // True when the same tests exist before and after, token for token, and no new syntax error appeared; otherwise adds an error.
    private static bool Verify(List<string> before, Dictionary<string, Output> outputs, IEnumerable<string> dests, Plan plan)
    {
        var destSet = dests.ToHashSet(StringComparer.Ordinal);
        var afterTrees = outputs.Where(o => destSet.Contains(o.Key) || !IsEmptyText(o.Value.Text))
            .Select(o => CSharpSyntaxTree.ParseText(o.Value.Text, Options)).ToList();
        var beforeTrees = before.Select(t => CSharpSyntaxTree.ParseText(t, Options)).ToList();
        var a = beforeTrees.SelectMany(t => TestMethods.In(t.GetRoot())).Select(Key).Order().ToList();
        var b = afterTrees.SelectMany(t => TestMethods.In(t.GetRoot())).Select(Key).Order().ToList();
        if (!a.SequenceEqual(b))
        {
            plan.Errors.Add("--split-tests left everything unchanged: the split would lose or alter a test");
            return false;
        }
        var hadErrors = beforeTrees.Any(HasErrors);
        if (!hadErrors && afterTrees.Any(HasErrors))
        {
            plan.Errors.Add("--split-tests left everything unchanged: the split would produce invalid C#");
            return false;
        }
        return true;
    }

    // A test's tokens, which ignore comments, markers and layout.
    private static string Key(MethodDeclarationSyntax method) => string.Join(" ", method.DescendantTokens().Select(t => t.Text));

    // True when the tree has a syntax error.
    private static bool HasErrors(SyntaxTree tree) => tree.GetDiagnostics().Any(d => d.Severity == DiagnosticSeverity.Error);

    // True when a file's text keeps nothing worth a file.
    private static bool IsEmptyText(string text) => TestFileText.IsEmpty(CSharpSyntaxTree.ParseText(text, Options).GetRoot());

    // Queues the writes, the deletions of emptied files, and one report line per source file.
    private static void Emit(string root, Dictionary<string, Output> outputs, HashSet<string> dests, Dictionary<SourceFile, Report> reports, Plan plan)
    {
        foreach (var (path, output) in outputs)
        {
            if (!dests.Contains(path) && IsEmptyText(output.Text))
                plan.Delete(path);
            else
                plan.Write(path, output.Text, output.Bom);
        }
        foreach (var (source, report) in reports)
            plan.Changes.Add($"{source.Rel}: " + Describe(root, report));
    }

    // The report line's text, such as "moved 3 tests to tests/Store/Add.cs; marked 1 unresolved test".
    private static string Describe(string root, Report report)
    {
        var parts = new List<string>();
        if (report.Moved > 0)
            parts.Add($"moved {report.Moved} tests to " + string.Join(", ", report.Targets.Select(t => Path.GetRelativePath(root, t).Replace('\\', '/'))));
        if (report.Marked > 0)
            parts.Add($"marked {report.Marked} tests whose method under test is unresolved");
        if (report.Cleared > 0)
            parts.Add($"removed {report.Cleared} stale unresolved markers");
        return string.Join("; ", parts);
    }

    // Drops every queued edit after an error, so nothing is half split.
    private static Plan Failed(Plan plan)
    {
        plan.Edits.Clear();
        plan.Changes.Clear();
        return plan;
    }
}
