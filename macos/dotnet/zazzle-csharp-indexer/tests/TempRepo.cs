// Test helper that creates a throwaway git repository with given files.
using ZazzleIndexer;

namespace ZazzleIndexer.Tests;

// A temporary git repo deleted on dispose.
public sealed class TempRepo : IDisposable
{
    public string Dir { get; } = Directory.CreateTempSubdirectory("zazzle-csharp-indexer").FullName;
    public Engine Engine { get; }

    // Creates the repo, writes the files and stages them.
    public TempRepo(Dictionary<string, string> files, Engine? engine = null, bool watch = false)
    {
        Engine = engine ?? Engine.Open(Directory.CreateTempSubdirectory("zazzle-csharp-indexer-state").FullName, watch);
        Proc.Run("git", ["-C", Dir, "init", "-q"], 20);
        foreach (var (rel, text) in files)
        {
            var full = Path.Combine(Dir, rel);
            Directory.CreateDirectory(Path.GetDirectoryName(full)!);
            File.WriteAllText(full, text);
        }
        Proc.Run("git", ["-C", Dir, "add", "-A"], 20);
    }

    // Opens the repo as a root named "r" and waits for the initial index.
    public Root Root()
    {
        var root = ZazzleIndexer.Root.Parse("r=" + Dir, Engine);
        root.Index.WaitReady(30000);
        return root;
    }

    // Deletes the directory.
    public void Dispose()
    {
        Engine.Dispose();
        Directory.Delete(Dir, true);
    }
}
