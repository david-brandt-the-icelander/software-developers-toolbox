// Change source built on macOS FSEvents with a persistent event id for journal replay.
using System.Runtime.InteropServices;

namespace ZazzleIndexer.Index;

// File-level FSEvents stream; replays history from a stored id after a restart.
public sealed class FsEventsSource : IChangeSource
{
    private const string CoreServices = "/System/Library/Frameworks/CoreServices.framework/CoreServices";
    private const string CoreFoundation = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation";
    private const uint FlagNoDefer = 0x2, FlagWatchRoot = 0x4, FlagFileEvents = 0x10;
    private const uint MustScan = 0x1 | 0x2 | 0x4 | 0x8 | 0x20;
    private const uint HistoryDoneFlag = 0x10;
    private const ulong SinceNow = ulong.MaxValue;

    private delegate void Callback(IntPtr stream, IntPtr info, nint count, IntPtr paths, IntPtr flags, IntPtr ids);

    private readonly Callback _callback;
    private IntPtr _stream;
    private long _lastId;
    private volatile bool _historyDone;

    public event Action<string[]>? Changed;
    public event Action? NeedsFullScan;

    public bool SupportsReplay => true;
    public bool HistoryDone => _historyDone;
    public long? LastEventId => _lastId == 0 ? null : _lastId;

    [DllImport(CoreServices)]
    private static extern IntPtr FSEventStreamCreate(IntPtr allocator, Callback callback, IntPtr context, IntPtr paths, ulong since, double latency, uint flags);

    [DllImport(CoreServices)]
    private static extern void FSEventStreamSetDispatchQueue(IntPtr stream, IntPtr queue);

    [DllImport(CoreServices)]
    private static extern byte FSEventStreamStart(IntPtr stream);

    [DllImport(CoreServices)]
    private static extern void FSEventStreamStop(IntPtr stream);

    [DllImport(CoreServices)]
    private static extern void FSEventStreamInvalidate(IntPtr stream);

    [DllImport(CoreServices)]
    private static extern void FSEventStreamRelease(IntPtr stream);

    [DllImport(CoreServices)]
    private static extern ulong FSEventsGetCurrentEventId();

    [DllImport(CoreFoundation)]
    private static extern IntPtr CFStringCreateWithCString(IntPtr alloc, [MarshalAs(UnmanagedType.LPUTF8Str)] string s, uint encoding);

    [DllImport(CoreFoundation)]
    private static extern IntPtr CFArrayCreate(IntPtr alloc, IntPtr[] values, nint count, IntPtr callbacks);

    [DllImport("/usr/lib/libSystem.dylib")]
    private static extern IntPtr dispatch_queue_create([MarshalAs(UnmanagedType.LPUTF8Str)] string label, IntPtr attr);

    // Creates the source; the delegate is kept in a field so the native callback stays valid.
    public FsEventsSource() => _callback = OnEvents;

    // Starts the stream from sinceId, or from now when no id is given.
    public void Start(string root, long? sinceId)
    {
        var lib = NativeLibrary.Load(CoreFoundation);
        var arrayCallbacks = NativeLibrary.GetExport(lib, "kCFTypeArrayCallBacks");
        var str = CFStringCreateWithCString(IntPtr.Zero, root, 0x08000100);
        var array = CFArrayCreate(IntPtr.Zero, [str], 1, arrayCallbacks);
        _lastId = sinceId ?? (long)FSEventsGetCurrentEventId();
        _historyDone = sinceId is null;
        var since = sinceId is { } id ? (ulong)id : SinceNow;
        _stream = FSEventStreamCreate(IntPtr.Zero, _callback, IntPtr.Zero, array, since, 0.05, FlagNoDefer | FlagWatchRoot | FlagFileEvents);
        if (_stream == IntPtr.Zero)
            throw new InvalidOperationException("FSEventStreamCreate failed");
        FSEventStreamSetDispatchQueue(_stream, dispatch_queue_create("zazzle-csharp-indexer.fsevents", IntPtr.Zero));
        if (FSEventStreamStart(_stream) == 0)
            throw new InvalidOperationException("FSEventStreamStart failed");
    }

    // Native callback: collects paths, tracks the newest id and flags that need a rescan.
    private void OnEvents(IntPtr stream, IntPtr info, nint count, IntPtr paths, IntPtr flags, IntPtr ids)
    {
        var changed = new List<string>();
        var rescan = false;
        for (var i = 0; i < count; i++)
        {
            var path = Marshal.PtrToStringUTF8(Marshal.ReadIntPtr(paths, i * IntPtr.Size)) ?? "";
            var flag = (uint)Marshal.ReadInt32(flags, i * 4);
            _lastId = Math.Max(_lastId, Marshal.ReadInt64(ids, i * 8));
            if ((flag & HistoryDoneFlag) != 0)
                _historyDone = true;
            else if ((flag & MustScan) != 0)
                rescan = true;
            else if (path.Length > 0)
                changed.Add(path);
        }
        if (changed.Count > 0)
            Changed?.Invoke(changed.ToArray());
        if (rescan)
            NeedsFullScan?.Invoke();
    }

    // Stops and releases the stream.
    public void Dispose()
    {
        if (_stream == IntPtr.Zero)
            return;
        FSEventStreamStop(_stream);
        FSEventStreamInvalidate(_stream);
        FSEventStreamRelease(_stream);
        _stream = IntPtr.Zero;
    }
}
