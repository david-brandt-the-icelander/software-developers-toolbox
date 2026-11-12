// Test helper that creates a throwaway git repository with given files.
using CodeIndexd;

namespace CodeIndexd.Tests;

// A temporary git repo deleted on dispose.
public sealed class TempRepo : IDisposable
{
    public string Dir { get; } = Directory.CreateTempSubdirectory("codeindexd").FullName;

    // Creates the repo, writes the files and stages them.
    public TempRepo(Dictionary<string, string> files)
    {
        Proc.Run("git", ["-C", Dir, "init", "-q"], 20);
        foreach (var (rel, text) in files)
        {
            var full = Path.Combine(Dir, rel);
            Directory.CreateDirectory(Path.GetDirectoryName(full)!);
            File.WriteAllText(full, text);
        }
        Proc.Run("git", ["-C", Dir, "add", "-A"], 20);
    }

    // Opens the repo as a root named "r".
    public Root Root() => CodeIndexd.Root.Parse("r=" + Dir);

    // Deletes the directory.
    public void Dispose() => Directory.Delete(Dir, true);
}
