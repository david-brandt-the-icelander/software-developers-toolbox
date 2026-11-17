// The changes one command wants to make, so they can be checked without writing or applied in one go.
using System.Text;

namespace ZazzleMinifier;

// One file to write (Bytes) or delete (null), by full path.
internal sealed record Edit(string Path, byte[]? Bytes);

// Collects a command's edits and the lines it reports, then applies them or lists them as violations.
internal sealed class Plan
{
    public List<Edit> Edits { get; } = [];

    // One line per source file the command changes, such as "foo/bar.cs: moved 3 comments to document/foo/bar.cs.md".
    public List<string> Changes { get; } = [];

    // Files passed over on purpose, such as generated code.
    public List<string> Notes { get; } = [];

    // Files left unchanged because something went wrong, with the reason.
    public List<string> Errors { get; } = [];

    // Queues a file write, keeping a byte-order mark when asked.
    public void Write(string path, string text, bool bom = false) =>
        Edits.Add(new Edit(path, [.. (bom ? Encoding.UTF8.Preamble : []), .. Encoding.UTF8.GetBytes(text)]));

    // Queues a file deletion.
    public void Delete(string path) => Edits.Add(new Edit(path, null));

    // Performs the edits in order, creating folders as needed and removing folders a deletion leaves empty.
    public void Apply()
    {
        foreach (var edit in Edits)
        {
            if (edit.Bytes == null)
            {
                File.Delete(edit.Path);
                var dir = Path.GetDirectoryName(edit.Path)!;
                if (Directory.Exists(dir) && !Directory.EnumerateFileSystemEntries(dir).Any())
                    Directory.Delete(dir);
                continue;
            }
            Directory.CreateDirectory(Path.GetDirectoryName(edit.Path)!);
            File.WriteAllBytes(edit.Path, edit.Bytes);
        }
    }

    // The .cs files this plan writes, so the next command can include files it created.
    public IEnumerable<string> WrittenSources() =>
        Edits.Where(e => e.Bytes != null && e.Path.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)).Select(e => e.Path);
}
