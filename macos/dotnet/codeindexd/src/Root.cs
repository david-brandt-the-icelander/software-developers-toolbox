// A directory inside a git repository that the server answers questions about.
using System.Text;

namespace CodeIndexd;

// A named root: tool paths are written as relative paths inside it.
public sealed class Root
{
    public string Name { get; }
    public string Path { get; }

    // Creates a root from an already validated name and absolute path.
    private Root(string name, string path)
    {
        Name = name;
        Path = path;
    }

    // Parses "name=path" or a bare path and checks that it is inside a git work tree.
    public static Root Parse(string spec)
    {
        var eq = spec.IndexOf('=');
        var path = System.IO.Path.GetFullPath(eq < 0 ? spec : spec[(eq + 1)..]);
        var name = eq < 0 ? System.IO.Path.GetFileName(path.TrimEnd('/')) : spec[..eq];
        if (!Directory.Exists(path))
            throw new ArgumentException($"not a directory: {path}");
        var inside = Proc.Run("git", ["-C", path, "rev-parse", "--is-inside-work-tree"], 10);
        if (inside.ExitCode != 0 || inside.Stdout.Trim() != "true")
            throw new ArgumentException($"not inside a git work tree: {path}");
        return new Root(name, path);
    }

    // Lists tracked and untracked-but-not-ignored files, relative to the root, sorted.
    public List<string> ListFiles()
    {
        var r = Proc.Run("git", ["-C", Path, "ls-files", "-co", "--exclude-standard", "-z"], 60);
        var files = r.Stdout.Split('\0', StringSplitOptions.RemoveEmptyEntries).Where(f => File.Exists(Full(f))).ToList();
        files.Sort(StringComparer.Ordinal);
        return files;
    }

    // Returns the absolute path for a relative path, or null if it escapes the root or is a symlink out of it.
    public string? Resolve(string relative)
    {
        var full = System.IO.Path.GetFullPath(System.IO.Path.Combine(Path, relative));
        var prefix = Path.TrimEnd('/') + "/";
        if (!full.StartsWith(prefix, StringComparison.Ordinal))
            return null;
        var real = new FileInfo(full).ResolveLinkTarget(true)?.FullName ?? full;
        return real.StartsWith(prefix, StringComparison.Ordinal) ? full : null;
    }

    // Joins a relative path onto the root path without validation.
    private string Full(string relative) => System.IO.Path.Combine(Path, relative);

    // Reports whether a file looks binary by looking for a NUL byte in its first 8 KiB.
    public static bool IsBinary(string fullPath)
    {
        using var s = File.OpenRead(fullPath);
        var buf = new byte[8192];
        var n = s.Read(buf, 0, buf.Length);
        return Array.IndexOf(buf, (byte)0, 0, n) >= 0;
    }

    // Decodes file bytes as UTF-8.
    public static string ReadText(string fullPath) => Encoding.UTF8.GetString(File.ReadAllBytes(fullPath));
}
