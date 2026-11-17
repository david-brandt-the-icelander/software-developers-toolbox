// Moves the files that follow renamed names (namespace folders, Type.cs files, test folders and files).
using System.Text.RegularExpressions;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace ZazzleMinifier;

// What one source file declares, read before the renames, to decide which renames move it.
internal sealed record FileFacts(HashSet<string> Namespaces, HashSet<string> Types, bool HasTests);

// Works out new paths for source files and queues their writes and moves.
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
        var collision = Collision(moves);
        if (collision != null)
        {
            plan.Changes.Clear();
            plan.Errors.Add($"renames left unchanged: {Rel(root, collision)} already exists");
            return;
        }
        Queue(root, workspace, texts, moves, plan);
    }

    // Writes changed and moved files first, then deletes the old paths deepest first so emptied folders go too.
    private static void Queue(string root, SourceWorkspace workspace, Dictionary<string, string> texts,
        Dictionary<string, string> moves, Plan plan)
    {
        var paths = texts.Keys.Concat(moves.Keys).Distinct().Order(StringComparer.Ordinal).ToList();
        foreach (var path in paths)
        {
            var to = moves.GetValueOrDefault(path, path);
            var file = workspace.Files[path];
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
        var projectDir = Layout.ProjectDir(root, original);
        if (projectDir == null)
            return path;
        var baseDir = projectDir;
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

    // Renames the folder segment that names the class under test (Outer.Inner style) when the file contains tests.
    private static string RenameTestFolder(string original, string path, IReadOnlyList<string> chain, string newName, Dictionary<string, FileFacts> facts)
    {
        if (!IsTestFile(original, facts))
            return path;
        var parts = path.Replace('\\', '/').Split('/');
        var classIndex = parts.Length - 2;
        var segments = parts[classIndex].Split('.');
        if (segments.Length < chain.Count || !segments.Take(chain.Count).SequenceEqual(chain))
            return path;
        segments[chain.Count - 1] = newName;
        parts[classIndex] = string.Join(".", segments);
        return string.Join("/", parts);
    }

    // Renames <ClassUnderTest>/<Member>.cs test files after the member.
    private static string MoveForMember(string original, string path, ResolvedRename rename, Dictionary<string, FileFacts> facts)
    {
        var parts = path.Replace('\\', '/').Split('/');
        var classIndex = parts.Length - 2;
        if (!IsTestFile(original, facts) || parts[^1] != rename.Request.Path[^1] + ".cs" || parts[classIndex] != string.Join(".", rename.TypeChain))
            return path;
        parts[^1] = rename.Request.NewName + ".cs";
        return string.Join("/", parts);
    }

    // True when the source file contains test methods.
    private static bool IsTestFile(string original, Dictionary<string, FileFacts> facts) => facts[original].HasTests;

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

    // The first destination that is taken: by a file that stays, or by two moves.
    private static string? Collision(Dictionary<string, string> moves)
    {
        var seen = new HashSet<string>(StringComparer.Ordinal);
        foreach (var to in moves.Values)
            if (!seen.Add(to) || (File.Exists(to) && !moves.ContainsKey(to)))
                return to;
        return null;
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

}
