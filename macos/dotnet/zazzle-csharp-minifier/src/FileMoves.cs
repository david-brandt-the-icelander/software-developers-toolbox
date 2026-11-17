// Moves the files that follow renamed names (namespace folders, Type.cs files, test folders and files) and updates comment-document headings.
using System.Text;
using System.Text.RegularExpressions;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// What one source file declares, read before the renames, to decide which renames move it.
internal sealed record FileFacts(HashSet<string> Namespaces, HashSet<string> Types, bool HasTests);

// Works out new paths for source files and their comment documents, and queues the writes, moves and heading updates.
internal static partial class FileMoves
{
    // Queues every file whose text or path changes, refusing the whole plan when a move would land on another file.
    public static void Plan(string root, SourceWorkspace workspace, List<ResolvedRename> renames, Dictionary<string, string> texts, Plan plan)
    {
        var facts = workspace.Files.ToDictionary(f => f.Key, f => Facts(f.Value.Text), StringComparer.Ordinal);
        var current = workspace.Files.Keys.ToDictionary(p => p, p => p, StringComparer.Ordinal);
        foreach (var rename in renames)
            foreach (var path in current.Keys.ToList())
                current[path] = Move(root, path, current[path], rename, facts);
        var moves = current.Where(m => m.Key != m.Value).ToDictionary(m => m.Key, m => m.Value, StringComparer.Ordinal);
        foreach (var (from, to) in moves.ToList())
            if (File.Exists(Layout.DocPath(root, from)))
                moves[Layout.DocPath(root, from)] = DocPathFor(root, from, to);
        var docs = Docs(root, renames);
        var collision = Collision(moves);
        if (collision != null)
        {
            plan.Changes.Clear();
            plan.Errors.Add($"renames left unchanged: {Rel(root, collision)} already exists");
            return;
        }
        Queue(root, workspace, texts, docs, moves, plan);
    }

    // Writes changed and moved files first, then deletes the old paths deepest first so emptied folders go too.
    private static void Queue(string root, SourceWorkspace workspace, Dictionary<string, string> texts, Dictionary<string, string> docs,
        Dictionary<string, string> moves, Plan plan)
    {
        var paths = texts.Keys.Concat(docs.Keys).Concat(moves.Keys).Distinct().Order(StringComparer.Ordinal).ToList();
        foreach (var path in paths)
        {
            var to = moves.GetValueOrDefault(path, path);
            if (docs.TryGetValue(path, out var doc) || !workspace.Files.TryGetValue(path, out var file))
                plan.Write(to, doc ?? File.ReadAllText(path));
            else
                plan.Write(to, texts.GetValueOrDefault(path, file.Text), file.Bom);
        }
        foreach (var (from, to) in moves.OrderByDescending(m => m.Key.Length))
        {
            plan.Delete(from);
            plan.Changes.Add($"moved {Rel(root, from)} to {Rel(root, to)}");
        }
    }

    // The path a file takes after one rename, given where earlier renames already put it.
    private static string Move(string root, string original, string path, ResolvedRename rename, Dictionary<string, FileFacts> facts) => rename.Kind switch
    {
        RenameKind.Namespace => MoveForNamespace(root, original, path, rename, facts),
        RenameKind.Type => MoveForType(original, path, rename, facts),
        _ => MoveForMember(original, path, rename, facts),
    };

    // Moves a file in the renamed namespace's folder (and its sub-folders) to the new folder, when that folder holds the namespace.
    private static string MoveForNamespace(string root, string original, string path, ResolvedRename rename, Dictionary<string, FileFacts> facts)
    {
        var (projectDir, baseDir) = Bases(root, original);
        if (projectDir == null)
            return path;
        var rootNs = RootNamespace(projectDir);
        var oldNs = rename.Request.Dotted;
        var newNs = string.Join(".", rename.Request.Path.SkipLast(1).Append(rename.Request.NewName));
        var oldFolder = NamespaceFolder(rootNs, oldNs);
        var newFolder = NamespaceFolder(rootNs, newNs);
        var rel = Path.GetRelativePath(baseDir, path).Replace('\\', '/');
        if (oldFolder.Length == 0 || newFolder.Length == 0 || !rel.StartsWith(oldFolder + "/", StringComparison.Ordinal))
            return path;
        if (!FolderHoldsNamespace(root, projectDir, Path.Combine(baseDir, oldFolder), oldNs, facts))
            return path;
        return Path.Combine(baseDir, newFolder + rel[oldFolder.Length..]);
    }

    // Renames Type.cs files that declare the type and the test folders named after it.
    private static string MoveForType(string original, string path, ResolvedRename rename, Dictionary<string, FileFacts> facts)
    {
        var name = Path.GetFileName(path);
        var dir = Path.GetDirectoryName(path)!;
        var fullType = string.Join(".", rename.Request.Path);
        if (name == rename.Request.Path[^1] + ".cs" && facts[original].Types.Contains(fullType))
            path = Path.Combine(dir, rename.Request.NewName + ".cs");
        return RenameTestFolder(original, path, rename.TypeChain, rename.Request.NewName, facts);
    }

    // Renames the folder segment that names the class under test (Outer.Inner style) when the file is a test file or its src/ half.
    private static string RenameTestFolder(string original, string path, IReadOnlyList<string> chain, string newName, Dictionary<string, FileFacts> facts)
    {
        if (!IsTestFile(original, facts))
            return path;
        var parts = path.Replace('\\', '/').Split('/');
        var classIndex = parts[^2] == Layout.Src && parts.Length > 2 ? parts.Length - 3 : parts.Length - 2;
        var segments = parts[classIndex].Split('.');
        if (segments.Length < chain.Count || !segments.Take(chain.Count).SequenceEqual(chain))
            return path;
        segments[chain.Count - 1] = newName;
        parts[classIndex] = string.Join(".", segments);
        return string.Join("/", parts);
    }

    // Renames <ClassUnderTest>/<Member>.cs test files (and their src/ halves) after the member.
    private static string MoveForMember(string original, string path, ResolvedRename rename, Dictionary<string, FileFacts> facts)
    {
        var parts = path.Replace('\\', '/').Split('/');
        var classIndex = parts[^2] == Layout.Src && parts.Length > 2 ? parts.Length - 3 : parts.Length - 2;
        if (!IsTestFile(original, facts) || parts[^1] != rename.Request.Path[^1] + ".cs" || parts[classIndex] != string.Join(".", rename.TypeChain))
            return path;
        parts[^1] = rename.Request.NewName + ".cs";
        return string.Join("/", parts);
    }

    // True for a file with tests, or the src/ half of one.
    private static bool IsTestFile(string original, Dictionary<string, FileFacts> facts)
    {
        if (facts[original].HasTests)
            return true;
        var dir = Path.GetDirectoryName(original)!;
        var header = Path.Combine(Path.GetDirectoryName(dir)!, Path.GetFileName(original));
        return Path.GetFileName(dir) == Layout.Src && facts.TryGetValue(header, out var h) && h.HasTests;
    }

    // The file's project folder and the folder its namespace folders start from: incl/ or src/ in a restructured project, the project folder otherwise.
    private static (string? ProjectDir, string BaseDir) Bases(string root, string path)
    {
        if (Layout.Restructured(root, path) is { } r && r.Folder is Layout.Incl or Layout.Src)
            return (r.ProjectDir, Path.Combine(r.ProjectDir, r.Folder));
        var project = Layout.ProjectDir(root, path);
        return (project, project ?? root);
    }

    // The project's RootNamespace, or its .csproj file name.
    private static string RootNamespace(string projectDir)
    {
        var csproj = Directory.EnumerateFiles(projectDir, "*.csproj").Order(StringComparer.Ordinal).First();
        var match = RootNamespaceElement().Match(File.ReadAllText(csproj));
        return match.Success ? match.Groups[1].Value.Trim() : Path.GetFileNameWithoutExtension(csproj);
    }

    // The folder path of a namespace inside its project: the part after the root namespace, dots as slashes; "" for the root namespace itself.
    private static string NamespaceFolder(string rootNs, string ns)
    {
        if (ns == rootNs)
            return "";
        var rest = ns.StartsWith(rootNs + ".", StringComparison.Ordinal) ? ns[(rootNs.Length + 1)..] : ns;
        return rest.Replace('.', '/');
    }

    // True when a file of this project in the folder (or below it) declares the namespace or one inside it.
    private static bool FolderHoldsNamespace(string root, string projectDir, string folder, string ns, Dictionary<string, FileFacts> facts) =>
        facts.Any(f => f.Key.StartsWith(folder + Path.DirectorySeparatorChar, StringComparison.Ordinal)
            && Layout.ProjectDir(root, f.Key) == projectDir
            && f.Value.Namespaces.Any(n => n == ns || n.StartsWith(ns + ".", StringComparison.Ordinal)));

    // Where a moved source file's comment document goes, mirroring Layout.DocPath for the new path.
    private static string DocPathFor(string root, string from, string to)
    {
        if (Layout.Restructured(root, from) is not { } r)
            return Path.Combine(root, Layout.DocFolder, Path.GetRelativePath(root, to) + ".md");
        var baseDir = r.Folder.Length == 0 ? r.ProjectDir : Path.Combine(r.ProjectDir, r.Folder);
        var rel = Path.GetRelativePath(baseDir, to).Replace('\\', '/');
        return Path.Combine(r.ProjectDir, Layout.Doc, r.Folder == Layout.Test ? $"{Layout.Test}/{rel}.md" : rel + ".md");
    }

    // The first destination that is taken: by a file that stays, or by two moves.
    private static string? Collision(Dictionary<string, string> moves)
    {
        var seen = new HashSet<string>(StringComparer.Ordinal);
        foreach (var to in moves.Values)
            if (!seen.Add(to) || (File.Exists(to) && !moves.ContainsKey(to)))
                return to;
        return null;
    }

    // The new text of every comment document whose headings name a renamed path.
    private static Dictionary<string, string> Docs(string root, List<ResolvedRename> renames)
    {
        var docs = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var path in DocFiles(root))
        {
            var text = File.ReadAllText(path);
            var updated = Heading().Replace(text, m => m.Groups[1].Value + RenameFqn(m.Groups[2].Value, renames) + "`");
            if (updated != text)
                docs[path] = updated;
        }
        return docs;
    }

    // Every comment document (*.cs.md) under the root, outside build output and hidden folders.
    private static IEnumerable<string> DocFiles(string root) =>
        Directory.EnumerateFiles(root, "*.cs.md", SearchOption.AllDirectories)
            .Where(p => !Path.GetRelativePath(root, p).Replace('\\', '/').Split('/').Any(s => s.StartsWith('.') || s is "bin" or "obj"));

    // The heading's qualified name with each renamed path's last name replaced, deepest rename first.
    private static string RenameFqn(string fqn, List<ResolvedRename> renames)
    {
        var segments = Segments(fqn);
        foreach (var rename in renames)
        {
            var path = rename.Request.Path;
            if (segments.Count < path.Count || !segments.Take(path.Count).Select(Bare).SequenceEqual(path))
                continue;
            var last = segments[path.Count - 1];
            segments[path.Count - 1] = rename.Request.NewName + last[Bare(last).Length..];
        }
        return string.Join(".", segments);
    }

    // Splits a qualified name on the dots outside <...> and (...).
    private static List<string> Segments(string fqn)
    {
        var segments = new List<string>();
        var sb = new StringBuilder();
        var depth = 0;
        foreach (var c in fqn)
        {
            depth += c is '<' or '(' or '[' ? 1 : c is '>' or ')' or ']' ? -1 : 0;
            if (c == '.' && depth == 0)
            {
                segments.Add(sb.ToString());
                sb.Clear();
                continue;
            }
            sb.Append(c);
        }
        segments.Add(sb.ToString());
        return segments;
    }

    // A segment's name without type parameters or parameters.
    private static string Bare(string segment)
    {
        var cut = segment.IndexOfAny(['<', '(', '[']);
        return cut < 0 ? segment : segment[..cut];
    }

    // What a source file declares, from its syntax.
    private static FileFacts Facts(string text)
    {
        var unit = CSharpSyntaxTree.ParseText(text, CSharpParseOptions.Default.WithLanguageVersion(LanguageVersion.Preview)).GetCompilationUnitRoot();
        var namespaces = unit.DescendantNodes().OfType<BaseNamespaceDeclarationSyntax>()
            .Select(n => string.Join(".", n.AncestorsAndSelf().OfType<BaseNamespaceDeclarationSyntax>().Reverse().Select(a => a.Name.ToString())))
            .ToHashSet(StringComparer.Ordinal);
        var types = unit.DescendantNodes().OfType<BaseTypeDeclarationSyntax>().Select(TypeName).ToHashSet(StringComparer.Ordinal);
        return new FileFacts(namespaces, types, TestMethods.In(unit).Count > 0);
    }

    // The dotted name of a type from the outermost namespace, nested types included.
    private static string TypeName(BaseTypeDeclarationSyntax type) =>
        string.Join(".", type.AncestorsAndSelf().Reverse().Select(n => n switch
        {
            BaseNamespaceDeclarationSyntax ns => ns.Name.ToString(),
            BaseTypeDeclarationSyntax t => t.Identifier.Text,
            _ => null,
        }).Where(n => n != null));

    // A path relative to the root with / separators.
    private static string Rel(string root, string path) => Path.GetRelativePath(root, path).Replace('\\', '/');

    // The <RootNamespace> element.
    [GeneratedRegex("<RootNamespace>([^<]+)</RootNamespace>")]
    private static partial Regex RootNamespaceElement();

    // A comment-document heading's prefix and backticked qualified name.
    [GeneratedRegex("(?m)^(## [^`\\n]*`)([^`\\n]+)`")]
    private static partial Regex Heading();
}
