// Locates the nearest C# project for workspace grouping and file moves.
namespace ZazzleMinifier;

// Resolves the project folder that contains a source file.
internal static class Layout
{
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

}
