// Where each kind of file belongs: beside its source (plain layout) or in a restructured project's incl/, src/, doc/ and test/ folders.
namespace ZazzleMinifier;

// Maps a source file to its implementation half, its comment document and its project, for both layouts.
internal static class Layout
{
    // The folders of a restructured project, relative to its .csproj.
    public const string Incl = "incl", Src = "src", Doc = "doc", Test = "test";

    // The folder under the root that mirrors plain-layout sources' comment documents.
    public const string DocFolder = "document";

    // The folder of the nearest .csproj at or above the file, stopping at the root; null when there is none.
    public static string? ProjectDir(string root, string path)
    {
        var dir = Directory.Exists(path) ? path : Path.GetDirectoryName(path);
        var top = Path.GetFullPath(root);
        while (dir != null && dir.Length >= top.Length)
        {
            if (Directory.EnumerateFiles(dir, "*.csproj").Any())
                return dir;
            dir = Path.GetDirectoryName(dir);
        }
        return null;
    }

    // True when the project compiles only incl/ and src/, the layout --restructure produces.
    public static bool IsRestructured(string projectDir)
    {
        var csproj = Directory.EnumerateFiles(projectDir, "*.csproj").FirstOrDefault();
        return csproj != null && File.ReadAllText(csproj).Contains($"Include=\"{Incl}/**/*.cs\"", StringComparison.Ordinal);
    }

    // The restructured project the file lives in, with the file's path below incl/, src/ or test/ ("" folder kept), or null in the plain layout.
    public static (string ProjectDir, string Folder, string Rel)? Restructured(string root, string path)
    {
        var project = ProjectDir(root, path);
        // The test project sits in test/ with its own .csproj; the restructured project is its parent.
        if (project != null && Path.GetFileName(project) == Test && Path.GetDirectoryName(project) is { } parent && IsRestructured(parent))
            project = parent;
        if (project == null || !IsRestructured(project))
            return null;
        var rel = Path.GetRelativePath(project, path).Replace('\\', '/');
        var slash = rel.IndexOf('/');
        var folder = slash < 0 ? "" : rel[..slash];
        return folder is Incl or Src or Test ? (project, folder, rel[(slash + 1)..]) : (project, "", rel);
    }

    // Where --split-headers puts a header's bodies: incl/<p> → src/<p> in a restructured project, D/N.cs → D/src/N.cs otherwise.
    public static string ImplPath(string root, string headerPath)
    {
        if (Restructured(root, headerPath) is { Folder: Incl } r)
            return Path.Combine(r.ProjectDir, Src, r.Rel);
        return Path.Combine(Path.GetDirectoryName(headerPath)!, Src, Path.GetFileName(headerPath));
    }

    // True for the implementation half of a split file, which the other commands leave alone: src/<p> with incl/<p> in a restructured project, D/src/N.cs with D/N.cs otherwise.
    public static bool IsImplementationHalf(string root, string path)
    {
        if (Restructured(root, path) is { } r)
            return r.Folder == Src && File.Exists(Path.Combine(r.ProjectDir, Incl, r.Rel));
        var dir = Path.GetDirectoryName(path)!;
        return Path.GetFileName(dir) == Src && File.Exists(Path.Combine(Path.GetDirectoryName(dir)!, Path.GetFileName(path)));
    }

    // Where --strip-comments writes a file's comments: doc/<p>.md in a restructured project (test files under doc/test/), document/<rel>.md under the root otherwise.
    public static string DocPath(string root, string sourcePath)
    {
        if (Restructured(root, sourcePath) is { } r)
            return Path.Combine(r.ProjectDir, Doc, r.Folder == Test ? $"{Test}/{r.Rel}.md" : r.Rel + ".md");
        return Path.Combine(root, DocFolder, Path.GetRelativePath(root, sourcePath) + ".md");
    }
}
