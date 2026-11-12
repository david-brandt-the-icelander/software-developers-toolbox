// Contract for a source of file-change notifications under one root.
namespace CodeIndexd.Index;

// Emits absolute changed paths; a source that can replay history reports an event id.
public interface IChangeSource : IDisposable
{
    event Action<string[]>? Changed;
    event Action? NeedsFullScan;

    bool SupportsReplay { get; }
    bool HistoryDone { get; }
    long? LastEventId { get; }

    // Starts watching; sinceId asks a replaying source for events after that id.
    void Start(string root, long? sinceId);
}
