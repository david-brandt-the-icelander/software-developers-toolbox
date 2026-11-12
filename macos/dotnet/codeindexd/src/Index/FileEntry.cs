// One indexed file of a root: its stat key and the blob holding its content analysis.
namespace CodeIndexd.Index;

// Immutable file record; BlobId 0 means the file was too large to analyse.
public sealed record FileEntry(string Path, long Size, long Mtime, long Birth, int BlobId)
{
    // Reports whether the file on disk still has the stored stat key.
    public bool Matches(FileInfo fi) =>
        fi.Length == Size && fi.LastWriteTimeUtc.Ticks == Mtime && fi.CreationTimeUtc.Ticks == Birth;

    // Builds an entry from the file's current stat key.
    public static FileEntry From(string path, FileInfo fi, int blobId) =>
        new(path, fi.Length, fi.LastWriteTimeUtc.Ticks, fi.CreationTimeUtc.Ticks, blobId);
}
