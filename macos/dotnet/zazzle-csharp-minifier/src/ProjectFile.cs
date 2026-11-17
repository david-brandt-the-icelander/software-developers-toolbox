// Reads and rewrites .csproj files for --restructure: test or production, project references, moved paths and the restructured compile items.
using System.Text.RegularExpressions;

namespace ZazzleMinifier;

// One .csproj: its path, folder, name, whether it is a test project and the full paths of the projects it references.
internal sealed record ProjectFile(string Path, string Dir, string Name, bool IsTest, List<string> References)
{
    // Package names whose reference marks a test project.
    private static readonly string[] TestPackages = ["Microsoft.NET.Test.Sdk", "xunit", "xunit.v3", "NUnit", "MSTest.TestFramework", "MSTest"];

    // Reads a .csproj from disk.
    public static ProjectFile Load(string path)
    {
        var text = File.ReadAllText(path);
        var dir = System.IO.Path.GetDirectoryName(path)!;
        var references = ProjectReference().Matches(text)
            .Select(m => System.IO.Path.GetFullPath(System.IO.Path.Combine(dir, m.Groups[1].Value.Replace('\\', '/'))))
            .ToList();
        return new ProjectFile(path, dir, System.IO.Path.GetFileNameWithoutExtension(path), IsTestText(text), references);
    }

    // True when the project says it is a test project or references a test framework.
    private static bool IsTestText(string text) =>
        Regex.IsMatch(text, @"<IsTestProject>\s*true\s*</IsTestProject>", RegexOptions.IgnoreCase)
        || PackageReference().Matches(text).Any(m => TestPackages.Contains(m.Groups[1].Value, StringComparer.OrdinalIgnoreCase));

    // The project text with every relative path that leaves its old folder re-pointed from the new folder, keeping each path's separator style.
    public static string Relocate(string text, string oldDir, string newDir)
    {
        text = PathAttribute().Replace(text, m => m.Groups[1].Value + "=\"" + RelocateList(m.Groups[2].Value, oldDir, newDir) + "\"");
        return HintPath().Replace(text, m => "<HintPath>" + RelocateList(m.Groups[1].Value, oldDir, newDir) + "</HintPath>");
    }

    // Re-points each ;-separated part of an attribute value that starts with "..".
    private static string RelocateList(string value, string oldDir, string newDir) =>
        string.Join(";", value.Split(';').Select(part => RelocateOne(part, oldDir, newDir)));

    // Re-points one relative path that climbs out of the old folder; leaves anything else as it was.
    private static string RelocateOne(string part, string oldDir, string newDir)
    {
        var trimmed = part.Trim();
        if (!trimmed.StartsWith("..", StringComparison.Ordinal))
            return part;
        var backslash = trimmed.Contains('\\');
        var full = System.IO.Path.GetFullPath(System.IO.Path.Combine(oldDir, trimmed.Replace('\\', '/')));
        var rel = System.IO.Path.GetRelativePath(newDir, full).Replace('\\', '/');
        return backslash ? rel.Replace('/', '\\') : rel;
    }

    // The production project text switched to the restructured layout: only incl/ and src/ compile, and test/ is left out of every default item.
    public static string Restructured(string text)
    {
        var nl = text.Contains("\r\n") ? "\r\n" : "\n";
        var indent = Indent(text);
        var properties = $"{indent}{indent}<EnableDefaultCompileItems>false</EnableDefaultCompileItems>{nl}"
            + $"{indent}{indent}<DefaultItemExcludes>$(DefaultItemExcludes);{Layout.Test}/**</DefaultItemExcludes>{nl}";
        var items = $"{indent}<ItemGroup>{nl}"
            + $"{indent}{indent}<Compile Include=\"{Layout.Incl}/**/*.cs\" />{nl}"
            + $"{indent}{indent}<Compile Include=\"{Layout.Src}/**/*.cs\" />{nl}"
            + $"{indent}</ItemGroup>{nl}";
        var close = text.IndexOf("</PropertyGroup>", StringComparison.Ordinal);
        if (close < 0)
            text = text.Insert(text.IndexOf('>', text.IndexOf("<Project", StringComparison.Ordinal)) + 1, $"{nl}{indent}<PropertyGroup>{nl}{indent}</PropertyGroup>");
        close = text.IndexOf("</PropertyGroup>", StringComparison.Ordinal);
        var lineStart = text.LastIndexOf('\n', close) + 1;
        text = text.Insert(lineStart, properties);
        var end = text.LastIndexOf("</Project>", StringComparison.Ordinal);
        return text.Insert(text.LastIndexOf('\n', end) + 1, items);
    }

    // True when the project lists its own Compile items, which the restructured globs may override.
    public static bool HasOwnCompileItems(string text) => Regex.IsMatch(text, "<Compile\\s+(Include|Remove|Update)=");

    // The indentation step the project file uses, two spaces when it has none.
    private static string Indent(string text)
    {
        var match = Regex.Match(text, "\n([ \t]+)<");
        return match.Success ? match.Groups[1].Value : "  ";
    }

    // A ProjectReference's Include value.
    private static Regex ProjectReference() => new("<ProjectReference\\s+Include=\"([^\"]+)\"");

    // A PackageReference's Include value.
    private static Regex PackageReference() => new("<PackageReference\\s+Include=\"([^\"]+)\"", RegexOptions.IgnoreCase);

    // An attribute that holds paths.
    private static Regex PathAttribute() => new("\\b(Include|Update|Remove|Project|Path)=\"([^\"]*)\"");

    // A HintPath element.
    private static Regex HintPath() => new("<HintPath>([^<]*)</HintPath>");
}
