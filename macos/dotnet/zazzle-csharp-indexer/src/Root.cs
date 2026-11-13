// A named directory inside a git repository that a session asks questions about.
using System.Text;
using ZazzleIndexer.Index;
using ZazzleIndexer.Tools;

namespace ZazzleIndexer;

// A session's alias for a root; tool paths are written root:relative/path.
public sealed class Root
{
    public string Name { get; }
    public string Path { get; }
    public RootIndex Index { get; }

    // Creates a root from a validated name, real path and its index.
    private Root(string name, string path, RootIndex index)
    {
        Name = name;
        Path = path;
        Index = index;
    }

    // Parses "name=path" or a bare path, checks it is inside a git work tree and attaches its index.
    public static Root Parse(string spec, Engine engine)
    {
        var eq = spec.IndexOf('=');
        var path = Native.RealPath(System.IO.Path.GetFullPath(eq < 0 ? spec : spec[(eq + 1)..]));
        var name = eq < 0 ? System.IO.Path.GetFileName(path.TrimEnd('/')) : spec[..eq];
        if (!Directory.Exists(path))
            throw new ArgumentException($"not a directory: {path}");
        var inside = Proc.Run("git", ["-C", path, "rev-parse", "--is-inside-work-tree"], 10);
        if (inside.ExitCode != 0 || inside.Stdout.Trim() != "true")
            throw new ArgumentException($"not inside a git work tree: {path}");
        return new Root(name, path, engine.Attach(path));
    }

    // Lists relative paths: from the index when it is ready, else straight from git (and says so in info).
    public List<string> Listing(CallInfo info)
    {
        if (Index.WaitReady(Engine.ReadyTimeoutMs))
            return Index.Files().Select(f => f.Path).ToList();
        info.Path = "scan";
        info.Reason = "index_cold";
        var r = Proc.Run("git", ["-C", Path, "ls-files", "-co", "--exclude-standard", "-z"], 120);
        var files = r.Stdout.Split('\0', StringSplitOptions.RemoveEmptyEntries).ToList();
        files.Sort(StringComparer.Ordinal);
        return files;
    }

    // Returns the absolute path for a relative path, or null if it (after resolving symlinks) is outside the root.
    public string? Resolve(string relative)
    {
        var full = System.IO.Path.GetFullPath(System.IO.Path.Combine(Path, relative));
        var prefix = Path.TrimEnd('/') + "/";
        if (!full.StartsWith(prefix, StringComparison.Ordinal))
            return null;
        return Native.RealPath(full).StartsWith(prefix, StringComparison.Ordinal) ? full : null;
    }

    // Decodes file bytes as UTF-8.
    public static string Decode(byte[] bytes) => Encoding.UTF8.GetString(bytes);
}
