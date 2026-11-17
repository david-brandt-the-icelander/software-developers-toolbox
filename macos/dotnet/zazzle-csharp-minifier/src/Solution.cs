// Finds the solutions under a root and the projects they list, and re-points a moved project in them.
using System.Text.RegularExpressions;

namespace ZazzleMinifier;

// Reads .sln and .slnx files.
internal static class Solution
{
    // Folder names never walked into while looking for solutions and projects.
    private static readonly HashSet<string> SkippedDirs = ["bin", "obj", "node_modules", "packages"];

    // The .sln and .slnx files under the root.
    public static List<string> Find(string root) =>
        Files(root, "*.sln").Concat(Files(root, "*.slnx")).Order(StringComparer.Ordinal).ToList();

    // The .csproj files the solutions list, or every .csproj under the root when there is no solution.
    public static List<string> Projects(string root)
    {
        var solutions = Find(root);
        if (solutions.Count == 0)
            return Files(root, "*.csproj").Order(StringComparer.Ordinal).ToList();
        return solutions.SelectMany(ProjectsOf).Where(File.Exists).Distinct().Order(StringComparer.Ordinal).ToList();
    }

    // The full paths of the .csproj files one solution lists.
    public static IEnumerable<string> ProjectsOf(string solution)
    {
        var dir = Path.GetDirectoryName(solution)!;
        return Entries().Matches(File.ReadAllText(solution))
            .Select(m => m.Groups["path"].Value)
            .Where(p => p.EndsWith(".csproj", StringComparison.OrdinalIgnoreCase))
            .Select(p => Path.GetFullPath(Path.Combine(dir, p.Replace('\\', '/'))));
    }

    // The solution text with the entry for one project pointing at its new path, or null when the solution does not list it.
    public static string? Repoint(string solution, string oldProject, string newProject)
    {
        var dir = Path.GetDirectoryName(solution)!;
        var text = File.ReadAllText(solution);
        var found = false;
        var result = Entries().Replace(text, m =>
        {
            var value = m.Groups["path"].Value;
            if (Path.GetFullPath(Path.Combine(dir, value.Replace('\\', '/'))) != oldProject)
                return m.Value;
            found = true;
            var rel = Path.GetRelativePath(dir, newProject).Replace('\\', '/');
            var replacement = value.Contains('\\') ? rel.Replace('/', '\\') : rel;
            var group = m.Groups["path"];
            return m.Value[..(group.Index - m.Index)] + replacement + m.Value[(group.Index - m.Index + group.Length)..];
        });
        return found ? result : null;
    }

    // The files matching a pattern under the root, leaving out build output and hidden folders.
    private static IEnumerable<string> Files(string root, string pattern)
    {
        foreach (var file in Directory.EnumerateFiles(root, pattern))
            yield return file;
        foreach (var sub in Directory.EnumerateDirectories(root))
        {
            var name = Path.GetFileName(sub);
            if (name.StartsWith('.') || SkippedDirs.Contains(name) || new DirectoryInfo(sub).LinkTarget != null)
                continue;
            foreach (var file in Files(sub, pattern))
                yield return file;
        }
    }

    // A project entry in a .sln (Project(...) = "Name", "path", "{GUID}") or a .slnx (<Project Path="path" />).
    private static Regex Entries() =>
        new("(^Project\\(\"\\{[^}]+\\}\"\\)\\s*=\\s*\"[^\"]*\",\\s*\"(?<path>[^\"]+)\")|(<Project\\s+Path=\"(?<path>[^\"]+)\")", RegexOptions.Multiline);
}
