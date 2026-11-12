// Process-wide state: the store, the shared blob table and the per-root indexes.
using System.Collections.Concurrent;
using CodeIndexd.Analysis;
using CodeIndexd.Index;

namespace CodeIndexd;

// Owns everything a daemon (or an in-process server) shares between sessions.
public sealed class Engine : IDisposable
{
    public const long MaxIndexBytes = 16 * 1024 * 1024;
    public static int ReadyTimeoutMs = 30000;

    private readonly ConcurrentDictionary<string, Lazy<RootIndex>> _roots = new();
    private readonly ConcurrentDictionary<string, BlobMeta> _byOid = new();
    private readonly ConcurrentDictionary<int, BlobMeta> _byId = new();
    private readonly object _blobLock = new();
    private readonly ConcurrentDictionary<string, HashSet<int>> _symbolNames = new();

    public Store Store { get; }
    public bool Watch { get; }
    public UsageLog Log { get; }
    public string StateDir { get; }
    public string Epoch { get; } = Guid.NewGuid().ToString("N")[..8];

    // Opens the store in the state directory and loads all blob facts into memory.
    private Engine(string stateDir, bool watch)
    {
        StateDir = stateDir;
        Watch = watch;
        Directory.CreateDirectory(stateDir);
        Store = new Store(System.IO.Path.Combine(stateDir, "store.db"));
        Log = new UsageLog(System.IO.Path.Combine(stateDir, "usage.jsonl"));
        foreach (var b in Store.LoadBlobs())
            Add(b);
        foreach (var (id, json) in Store.AllSymbols())
            IndexSymbols(id, Sym.FromJson(json));
    }

    // Finds blob ids declaring a symbol whose lowercase name equals (exact) or contains the query.
    public HashSet<int> BlobsDeclaring(string name, bool exact)
    {
        var q = name.ToLowerInvariant();
        var result = new HashSet<int>();
        foreach (var (k, ids) in _symbolNames)
            if (exact ? k == q : k.Contains(q))
                lock (ids)
                    result.UnionWith(ids);
        return result;
    }

    // Adds a blob's symbol names to the name index.
    private void IndexSymbols(int blobId, List<Sym> syms)
    {
        foreach (var s in syms)
        {
            var set = _symbolNames.GetOrAdd(s.Name.ToLowerInvariant(), _ => []);
            lock (set)
                set.Add(blobId);
        }
    }

    // Opens an engine; the state directory defaults to the platform location.
    public static Engine Open(string? stateDir = null, bool watch = true) => new(stateDir ?? UsageLog.DefaultDir(), watch);

    // Returns the index for a root path, creating and starting it on first use.
    public RootIndex Attach(string path)
    {
        var real = Native.RealPath(path);
        return _roots.GetOrAdd(real, p => new Lazy<RootIndex>(() =>
        {
            var idx = new RootIndex(this, p);
            idx.Start();
            return idx;
        })).Value;
    }

    // Returns blob facts by id.
    public BlobMeta? Blob(int id) => _byId.GetValueOrDefault(id);

    // Returns the blob for these bytes, analysing and storing it first if its content is new.
    public BlobMeta GetOrAddBlob(byte[] bytes, string path)
    {
        var oid = BlobAnalyzer.GitOid(bytes);
        if (_byOid.TryGetValue(oid, out var hit))
            return hit;
        var d = BlobAnalyzer.Analyze(bytes, path);
        lock (_blobLock)
        {
            if (_byOid.TryGetValue(oid, out hit))
                return hit;
            var id = Store.InsertBlob(d.Oid, d.Size, d.Binary, d.Lang, d.Lines, d.Bloom, d.Digest, d.Symbols, d.Idents);
            var meta = new BlobMeta(id, d.Oid, d.Size, d.Binary, d.Lang, d.Lines, d.Bloom, d.Digest);
            Add(meta);
            IndexSymbols(id, Sym.FromJson(d.Symbols));
            return meta;
        }
    }

    // Registers blob facts in both lookup tables.
    private void Add(BlobMeta b)
    {
        _byOid[b.Oid] = b;
        _byId[b.Id] = b;
    }

    // Stops every root.
    public void Dispose()
    {
        foreach (var r in _roots.Values.Where(l => l.IsValueCreated))
            r.Value.Dispose();
    }
}
