// Live index of one git work tree: file stat keys, blob links, change log and watcher.
using System.Collections.Concurrent;

namespace CodeIndexd.Index;

// Keeps a root's files current from git listings, stat keys and change notifications.
public sealed class RootIndex : IDisposable
{
    private const int MaxLog = 50000;
    private static readonly long ReplayMaxAgeTicks = TimeSpan.FromHours(24).Ticks;

    private readonly Engine _engine;
    private readonly ConcurrentDictionary<string, FileEntry> _files = new();
    private readonly ConcurrentQueue<string> _pending = new();
    private readonly object _reconcileLock = new();
    private readonly object _logLock = new();
    private readonly List<(long gen, string path, char kind)> _log = [];
    private readonly Timer _debounce;
    private readonly Timer _periodic;
    private IChangeSource? _source;
    private long _gen;
    private long _logBase;
    private volatile bool _stopped;

    public string Path { get; }
    public int Id { get; }
    public Task Ready { get; private set; } = Task.CompletedTask;

    // Creates the index and loads stored file records; call Start to begin tracking.
    public RootIndex(Engine engine, string path)
    {
        _engine = engine;
        Path = path;
        Id = engine.Store.RootId(path);
        foreach (var e in engine.Store.LoadFiles(Id))
            _files[e.Path] = e;
        _debounce = new Timer(_ => Drain(), null, Timeout.Infinite, Timeout.Infinite);
        _periodic = new Timer(_ => Safely(Reconcile), null, Timeout.Infinite, Timeout.Infinite);
    }

    // Starts the watcher and the initial reconcile; Ready completes when the index is current.
    public void Start()
    {
        var (eventId, fullScan) = _engine.Store.RootState(Id);
        _source = _engine.Watch ? ChangeSources.Create() : null;
        var recent = fullScan is { } t && DateTime.UtcNow.Ticks - t < ReplayMaxAgeTicks;
        var replay = _source is { SupportsReplay: true } && eventId is not null && recent;
        if (_source is not null)
        {
            _source.Changed += OnChanged;
            _source.NeedsFullScan += () => _debounce.Change(100, Timeout.Infinite);
            _source.Start(Path, replay ? eventId : null);
        }
        Ready = Task.Run(() =>
        {
            var replayed = replay && SpinWait.SpinUntil(() => _source!.HistoryDone, 10000);
            if (replayed)
                Drain();
            else
                Reconcile();
            if (_source is not null)
                _periodic.Change(TimeSpan.FromMinutes(5), TimeSpan.FromMinutes(5));
        });
    }

    // Waits until the initial reconcile has finished; false means it is still running.
    public bool WaitReady(int ms) => Ready.Wait(ms);

    // Current generation: increases with every change.
    public long Gen => Interlocked.Read(ref _gen);

    // Returns a snapshot of all files sorted by path.
    public List<FileEntry> Files()
    {
        var list = _files.Values.ToList();
        list.Sort((a, b) => string.CompareOrdinal(a.Path, b.Path));
        return list;
    }

    // Looks up one file by relative path.
    public FileEntry? Get(string rel) => _files.GetValueOrDefault(rel);

    // Re-checks one file against the disk now and returns its current entry, or null if it is gone.
    public FileEntry? Freshen(string rel)
    {
        Analyze(rel);
        return _files.GetValueOrDefault(rel);
    }

    // Lists changes after a generation; expired is true when the log no longer reaches back that far.
    public List<(string path, char kind)> ChangesSince(long gen, out bool expired)
    {
        lock (_logLock)
        {
            expired = gen < _logBase;
            return _log.Where(l => l.gen > gen).GroupBy(l => l.path).Select(g => (g.Key, g.Last().kind)).ToList();
        }
    }

    // Runs a full reconcile now (used by tests and as the safety net).
    public void Sync() => Reconcile();

    // Lists git-visible files, analyses new or changed ones, and drops vanished ones.
    private void Reconcile()
    {
        lock (_reconcileLock)
        {
            var listing = GitFiles();
            var work = new List<string>();
            var present = new HashSet<string>();
            foreach (var p in listing)
            {
                var fi = new FileInfo(System.IO.Path.Combine(Path, p));
                if (!fi.Exists || fi.LinkTarget is not null)
                    continue;
                present.Add(p);
                if (_files.TryGetValue(p, out var e) && e.BlobId != -1 && e.Matches(fi))
                    continue;
                work.Add(p);
            }
            foreach (var known in _files.Keys.Where(k => !present.Contains(k)).ToList())
                Remove(known);
            Parallel.ForEach(work, new ParallelOptions { MaxDegreeOfParallelism = Math.Max(1, Environment.ProcessorCount - 1) }, Analyze);
            _engine.Store.SetFullScan(Id, DateTime.UtcNow.Ticks);
            Persist();
        }
    }

    // Returns relative paths of tracked and untracked-but-not-ignored files.
    private List<string> GitFiles()
    {
        var r = Proc.Run("git", ["-C", Path, "ls-files", "-co", "--exclude-standard", "-z"], 120);
        return r.Stdout.Split('\0', StringSplitOptions.RemoveEmptyEntries).ToList();
    }

    // Re-reads one file if its stat key changed and records the result.
    private void Analyze(string rel)
    {
        var fi = new FileInfo(System.IO.Path.Combine(Path, rel));
        if (!fi.Exists || fi.LinkTarget is not null)
        {
            Remove(rel);
            return;
        }
        var known = _files.GetValueOrDefault(rel);
        if (known is { BlobId: not -1 } && known.Matches(fi))
            return;
        var blob = fi.Length <= Engine.MaxIndexBytes ? _engine.GetOrAddBlob(File.ReadAllBytes(fi.FullName), rel) : null;
        var entry = FileEntry.From(rel, fi, blob?.Id ?? 0);
        _files[rel] = entry;
        _engine.Store.UpsertFile(Id, entry);
        Log(rel, known is null ? 'A' : 'M');
    }

    // Forgets a file.
    private void Remove(string rel)
    {
        if (!_files.TryRemove(rel, out _))
            return;
        _engine.Store.DeleteFile(Id, rel);
        Log(rel, 'D');
    }

    // Appends to the change log and bumps the generation.
    private void Log(string rel, char kind)
    {
        var gen = Interlocked.Increment(ref _gen);
        lock (_logLock)
        {
            _log.Add((gen, rel, kind));
            if (_log.Count > MaxLog)
            {
                _logBase = _log[MaxLog / 2].gen;
                _log.RemoveRange(0, MaxLog / 2);
            }
        }
    }

    // Handles raw watcher paths: queues them and starts the debounce timer.
    private void OnChanged(string[] paths)
    {
        foreach (var p in paths)
            _pending.Enqueue(p);
        _debounce.Change(100, Timeout.Infinite);
    }

    // Applies queued watcher paths; unknown paths are checked against ignore rules in one git call.
    private void Drain()
    {
        if (_stopped)
            return;
        var full = false;
        var unknown = new HashSet<string>();
        while (_pending.TryDequeue(out var abs))
        {
            var rel = System.IO.Path.GetRelativePath(Path, abs).Replace('\\', '/');
            if (rel.StartsWith("../") || rel == ".")
                continue;
            if (rel.StartsWith(".git/") || rel == ".git")
                full |= rel is ".git/HEAD" or ".git/info/exclude";
            else if (rel.EndsWith(".gitignore"))
                full = true;
            else if (_files.ContainsKey(rel))
                Safely(() => Analyze(rel));
            else
                unknown.Add(rel);
        }
        full |= HandleUnknown(unknown);
        if (full)
            Safely(Reconcile);
        else
            Persist();
    }

    // Analyses unknown non-ignored files; returns true when a new directory needs a full reconcile.
    private bool HandleUnknown(HashSet<string> unknown)
    {
        if (unknown.Count == 0)
            return false;
        var ignored = IgnoredAmong(unknown);
        var needFull = false;
        foreach (var rel in unknown.Where(u => !ignored.Contains(u)))
        {
            var abs = System.IO.Path.Combine(Path, rel);
            if (Directory.Exists(abs))
                needFull = true;
            else if (File.Exists(abs))
                Safely(() => Analyze(rel));
        }
        return needFull;
    }

    // Asks git which of the paths are ignored.
    private HashSet<string> IgnoredAmong(IEnumerable<string> paths)
    {
        var psi = new System.Diagnostics.ProcessStartInfo("git") { RedirectStandardInput = true, RedirectStandardOutput = true, UseShellExecute = false };
        foreach (var a in new[] { "-C", Path, "check-ignore", "--stdin", "-z" })
            psi.ArgumentList.Add(a);
        using var p = System.Diagnostics.Process.Start(psi)!;
        var read = Task.Run(() => p.StandardOutput.ReadToEnd());
        p.StandardInput.Write(string.Join('\0', paths) + "\0");
        p.StandardInput.Close();
        p.WaitForExit(20000);
        return read.Result.Split('\0', StringSplitOptions.RemoveEmptyEntries).ToHashSet();
    }

    // Stores the watcher's latest event id.
    private void Persist()
    {
        if (_source?.LastEventId is { } id)
            _engine.Store.SetEventId(Id, id);
    }

    // Runs an action and swallows I/O races caused by files changing under us.
    private static void Safely(Action a)
    {
        try
        {
            a();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
        }
    }

    // Stops watching and timers.
    public void Dispose()
    {
        _stopped = true;
        _debounce.Dispose();
        _periodic.Dispose();
        Persist();
        _source?.Dispose();
    }
}

// Chooses the change source for the platform.
public static class ChangeSources
{
    // Returns FSEvents on macOS and FileSystemWatcher elsewhere.
    public static IChangeSource Create() => OperatingSystem.IsMacOS() ? new FsEventsSource() : new FswSource();
}
