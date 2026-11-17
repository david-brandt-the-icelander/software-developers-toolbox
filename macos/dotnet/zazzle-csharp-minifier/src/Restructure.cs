// The --restructure command: turns each production project of the solution into <project>/incl, src, doc and test.
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// Plans, project by project, the moves into incl/, src/, doc/ and test/, the project and solution edits, and the .ignore file.
internal static class Restructure
{
    // Folder names never walked into.
    private static readonly HashSet<string> SkippedDirs = ["bin", "obj", "node_modules", "packages"];

    // The .ignore text that keeps broad searches in incl/.
    public const string IgnoreText = "# zazzle-csharp-minifier: agents search incl/ only; open src/, doc/, test/ files by exact path\n/src/\n/doc/\n/test/\n";

    // One file to move, with the bytes it carries (a rewritten project file carries its new text).
    private sealed record Move(string From, string To, byte[] Bytes);

    // The edits that restructure every production project that holds one of the files.
    public static Plan Make(string root, IReadOnlyList<string> files)
    {
        var plan = new Plan();
        var projects = SolutionFiles.Projects(root).Select(ProjectFile.Load).ToList();
        var production = projects.Where(p => !p.IsTest).ToList();
        var testsOf = AssignTests(production, projects.Where(p => p.IsTest), plan);
        foreach (var project in production)
        {
            var test = testsOf.GetValueOrDefault(project.Path);
            if (Touches(project, test, files))
                One(root, project, test, plan);
        }
        return plan;
    }

    // Gives each production project at most one test project: the only one referencing it, or the one named after it.
    private static Dictionary<string, ProjectFile> AssignTests(List<ProjectFile> production, IEnumerable<ProjectFile> tests, Plan plan)
    {
        var result = new Dictionary<string, ProjectFile>();
        foreach (var test in tests)
        {
            var refs = production.Where(p => test.References.Contains(p.Path)).ToList();
            if (refs.Count > 1)
                refs = refs.Where(p => test.Name.StartsWith(p.Name, StringComparison.Ordinal)).OrderByDescending(p => p.Name.Length).Take(1).ToList();
            if (refs.Count == 0)
            {
                plan.Notes.Add($"{test.Name}: left in place, no single production project it tests");
                continue;
            }
            if (!result.TryAdd(refs[0].Path, test))
                plan.Notes.Add($"{test.Name}: left in place, {refs[0].Name} already has test project {result[refs[0].Path].Name}");
        }
        return result;
    }

    // True when no files were named, or one of them lies in the project or its test project.
    private static bool Touches(ProjectFile project, ProjectFile? test, IReadOnlyList<string> files) =>
        files.Count == 0 || files.Any(f => Under(project.Dir, f) || (test != null && Under(test.Dir, f)));

    // True when the path is inside the folder.
    private static bool Under(string dir, string path)
    {
        var rel = Path.GetRelativePath(dir, path);
        return !rel.StartsWith("..", StringComparison.Ordinal) && !Path.IsPathRooted(rel);
    }

    // Plans one project: sources, comment documents, the test project, the project file and .ignore; nothing when a check fails.
    private static void One(string root, ProjectFile project, ProjectFile? test, Plan plan)
    {
        var rel = Path.GetRelativePath(root, project.Dir).Replace('\\', '/');
        var restructured = Layout.IsRestructured(project.Dir);
        var moves = SourceMoves(project, restructured).ToList();
        var sourceCount = moves.Count;
        moves.AddRange(DocMoves(root, project.Dir, Path.Combine(project.Dir, Layout.Doc)));
        var docCount = moves.Count - sourceCount;
        var solutions = new List<(string Path, string Text)>();
        if (test != null && test.Dir != Path.Combine(project.Dir, Layout.Test))
        {
            moves.AddRange(TestMoves(test, Path.Combine(project.Dir, Layout.Test)));
            var docsBefore = moves.Count;
            moves.AddRange(DocMoves(root, test.Dir, Path.Combine(project.Dir, Layout.Doc, Layout.Test)));
            docCount += moves.Count - docsBefore;
            solutions = SolutionEdits(root, test, Path.Combine(project.Dir, Layout.Test, Path.GetFileName(test.Path)));
        }
        moves = moves.Where(m => m.From != m.To).ToList();
        var csprojText = File.ReadAllText(project.Path);
        var newCsproj = restructured ? csprojText : ProjectFile.Restructured(csprojText);
        var ignorePath = Path.Combine(project.Dir, ".ignore");
        var ignoreChanged = !File.Exists(ignorePath) || File.ReadAllText(ignorePath) != IgnoreText;
        if (moves.Count == 0 && newCsproj == csprojText && !ignoreChanged)
            return;
        if (Problem(moves) is { } problem)
        {
            plan.Errors.Add($"{rel}: left unchanged: {problem}");
            return;
        }
        Queue(plan, moves, solutions);
        if (newCsproj != csprojText)
            plan.Write(project.Path, newCsproj);
        if (ignoreChanged)
            plan.Write(ignorePath, IgnoreText);
        if (!restructured && ProjectFile.HasOwnCompileItems(csprojText))
            plan.Notes.Add($"{rel}: its own Compile items still name the old paths; check them against incl/ and src/");
        plan.Changes.Add($"{rel}: restructured into {Count(moves, project.Dir, Layout.Incl)}, {Count(moves, project.Dir, Layout.Src)}, doc/ ({docCount}), {Count(moves, project.Dir, Layout.Test)}");
    }

    // "incl/ (N)": how many moves land in that folder of the project.
    private static string Count(List<Move> moves, string projectDir, string folder) =>
        $"{folder}/ ({moves.Count(m => Under(Path.Combine(projectDir, folder), m.To))})";

    // Where each source of the project goes: implementation halves to src/, top-level programs to src/, everything else to incl/.
    private static IEnumerable<Move> SourceMoves(ProjectFile project, bool restructured)
    {
        var skip = NestedProjects(project.Dir).ToHashSet();
        if (restructured)
            skip.UnionWith(new[] { Layout.Incl, Layout.Src, Layout.Test, Layout.Doc }.Select(f => Path.Combine(project.Dir, f)));
        foreach (var file in Walk(project.Dir, skip).Where(f => f.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)))
            yield return new Move(file, SourceDest(project.Dir, file), File.ReadAllBytes(file));
    }

    // The destination of one source file in the project.
    private static string SourceDest(string projectDir, string file)
    {
        var dir = Path.GetDirectoryName(file)!;
        if (Path.GetFileName(dir) == Layout.Src && File.Exists(Path.Combine(Path.GetDirectoryName(dir)!, Path.GetFileName(file))))
            return Path.GetFullPath(Path.Combine(projectDir, Layout.Src, Path.GetRelativePath(projectDir, Path.GetDirectoryName(dir)!), Path.GetFileName(file)));
        var folder = HasTopLevelStatements(file) ? Layout.Src : Layout.Incl;
        return Path.Combine(projectDir, folder, Path.GetRelativePath(projectDir, file));
    }

    // True when the file is a program written as top-level statements.
    private static bool HasTopLevelStatements(string file) =>
        CSharpSyntaxTree.ParseText(File.ReadAllText(file)).GetRoot() is CompilationUnitSyntax unit && unit.Members.OfType<GlobalStatementSyntax>().Any();

    // Moves of the comment documents --strip-comments wrote under document/ for a folder into the given doc folder, keeping their paths.
    private static IEnumerable<Move> DocMoves(string root, string sourceDir, string docDir)
    {
        var mirror = Path.Combine(root, Layout.DocFolder, Path.GetRelativePath(root, sourceDir));
        if (!Directory.Exists(mirror))
            yield break;
        var skip = NestedProjects(sourceDir).Select(d => Path.Combine(mirror, Path.GetRelativePath(sourceDir, d))).ToHashSet();
        foreach (var doc in Walk(mirror, skip).Where(f => f.EndsWith(".md", StringComparison.Ordinal)))
        {
            var rel = Path.GetRelativePath(mirror, doc).Replace('\\', '/');
            yield return new Move(doc, Path.Combine(docDir, rel), File.ReadAllBytes(doc));
        }
    }

    // Moves of every file of the test project into test/, its project file re-pointed from there.
    private static IEnumerable<Move> TestMoves(ProjectFile test, string newDir)
    {
        foreach (var file in Walk(test.Dir, NestedProjects(test.Dir).ToHashSet()))
        {
            var to = Path.Combine(newDir, Path.GetRelativePath(test.Dir, file));
            var bytes = file == test.Path
                ? System.Text.Encoding.UTF8.GetBytes(ProjectFile.Relocate(File.ReadAllText(file), test.Dir, newDir))
                : File.ReadAllBytes(file);
            yield return new Move(file, to, bytes);
        }
    }

    // The solution files that list the test project, with its entry re-pointed.
    private static List<(string Path, string Text)> SolutionEdits(string root, ProjectFile test, string newPath) =>
        SolutionFiles.Find(root).Select(s => (s, SolutionFiles.Repoint(s, test.Path, newPath))).Where(e => e.Item2 != null).Select(e => (e.s, e.Item2!)).ToList();

    // Why the moves cannot be made safely: two files to one place, or a file in the way; null when they can.
    private static string? Problem(List<Move> moves)
    {
        var dup = moves.GroupBy(m => m.To).FirstOrDefault(g => g.Count() > 1);
        if (dup != null)
            return $"{string.Join(" and ", dup.Select(m => m.From))} would both move to {dup.Key}";
        var leaving = moves.Select(m => m.From).ToHashSet();
        var blocked = moves.FirstOrDefault(m => File.Exists(m.To) && !leaving.Contains(m.To));
        return blocked == null ? null : $"{blocked.To} is in the way of {blocked.From}";
    }

    // Queues the writes before any deletion, so a crash never loses a file, and skips deleting a path that is also a destination.
    private static void Queue(Plan plan, List<Move> moves, List<(string Path, string Text)> solutions)
    {
        foreach (var move in moves)
            plan.Edits.Add(new Edit(move.To, move.Bytes));
        foreach (var (path, text) in solutions)
            plan.Write(path, text);
        var arriving = moves.Select(m => m.To).ToHashSet();
        foreach (var move in moves.Where(m => !arriving.Contains(m.From)))
            plan.Delete(move.From);
    }

    // The folders below dir that hold a project of their own.
    private static IEnumerable<string> NestedProjects(string dir) =>
        Walk(dir, []).Where(f => f.EndsWith(".csproj", StringComparison.OrdinalIgnoreCase))
            .Select(f => Path.GetDirectoryName(f)!).Where(d => d != dir).Distinct();

    // Every file under dir, leaving out build output, hidden folders, symlinked folders and the skipped folders.
    private static IEnumerable<string> Walk(string dir, HashSet<string> skip)
    {
        foreach (var file in Directory.EnumerateFiles(dir))
            yield return file;
        foreach (var sub in Directory.EnumerateDirectories(dir))
        {
            var name = Path.GetFileName(sub);
            if (skip.Contains(sub) || name.StartsWith('.') || SkippedDirs.Contains(name) || new DirectoryInfo(sub).LinkTarget != null)
                continue;
            foreach (var file in Walk(sub, skip))
                yield return file;
        }
    }
}
