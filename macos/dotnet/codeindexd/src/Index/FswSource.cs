// Change source built on .NET FileSystemWatcher, used where FSEvents is unavailable.
namespace CodeIndexd.Index;

// Recursive watcher with no history replay.
public sealed class FswSource : IChangeSource
{
    private FileSystemWatcher? _w;

    public event Action<string[]>? Changed;
    public event Action? NeedsFullScan;

    public bool SupportsReplay => false;
    public bool HistoryDone => true;
    public long? LastEventId => null;

    // Starts a recursive watcher on the root.
    public void Start(string root, long? sinceId)
    {
        _w = new FileSystemWatcher(root)
        {
            IncludeSubdirectories = true,
            InternalBufferSize = 64 * 1024,
            NotifyFilter = NotifyFilters.FileName | NotifyFilters.DirectoryName | NotifyFilters.LastWrite | NotifyFilters.Size | NotifyFilters.CreationTime,
        };
        _w.Created += (_, e) => Changed?.Invoke([e.FullPath]);
        _w.Changed += (_, e) => Changed?.Invoke([e.FullPath]);
        _w.Deleted += (_, e) => Changed?.Invoke([e.FullPath]);
        _w.Renamed += (_, e) => Changed?.Invoke([e.OldFullPath, e.FullPath]);
        _w.Error += (_, _) => NeedsFullScan?.Invoke();
        _w.EnableRaisingEvents = true;
    }

    // Stops watching.
    public void Dispose() => _w?.Dispose();
}
