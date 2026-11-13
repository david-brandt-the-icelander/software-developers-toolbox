// Append-only JSONL record of every tool call, kept locally to guide later versions.
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;

namespace ZazzleIndexer;

// What one tool call did, filled in by the tool.
public sealed class CallInfo
{
    public string Path = "index";
    public string? Reason;
    public int Results;
    public bool Truncated;
    public int Candidates = -1;
    public int TotalFiles = -1;
    public string? Note;
}

// Thread-safe JSONL writer; a null path disables logging.
public sealed class UsageLog
{
    private const long RotateBytes = 20 * 1024 * 1024;
    private readonly string? _file;
    private readonly bool _plain = Environment.GetEnvironmentVariable("ZAZZLE_INDEXER_LOG_PLAIN") == "1";
    private readonly object _lock = new();

    // Creates a log writing to the given file.
    public UsageLog(string? file) => _file = file;

    // Opens the log in the state directory (ZAZZLE_INDEXER_STATE_DIR overrides it).
    public static UsageLog Open()
    {
        var dir = DefaultDir();
        try
        {
            Directory.CreateDirectory(dir);
            return new UsageLog(System.IO.Path.Combine(dir, "usage.jsonl"));
        }
        catch (IOException)
        {
            return new UsageLog(null);
        }
    }

    // Picks the state directory: ZAZZLE_INDEXER_STATE_DIR, else the platform default.
    public static string DefaultDir()
    {
        if (Environment.GetEnvironmentVariable("ZAZZLE_INDEXER_STATE_DIR") is { Length: > 0 } over)
            return over;
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        if (OperatingSystem.IsMacOS())
            return System.IO.Path.Combine(home, "Library", "Application Support", "zazzle-csharp-indexer");
        var xdg = Environment.GetEnvironmentVariable("XDG_STATE_HOME");
        return System.IO.Path.Combine(string.IsNullOrEmpty(xdg) ? System.IO.Path.Combine(home, ".local", "state") : xdg, "zazzle-csharp-indexer");
    }

    // Writes one record; arguments are reduced to their shape, with free-text values hashed unless plain logging is on.
    public void Record(string session, string tool, JsonNode? args, CallInfo info, long ms, int bytesOut, string? error)
    {
        if (_file is null)
            return;
        var rec = new JsonObject
        {
            ["v"] = 1,
            ["ts"] = DateTime.UtcNow.ToString("o"),
            ["session"] = session,
            ["tool"] = tool,
            ["args"] = Shape(args),
            ["path"] = info.Path,
            ["reason"] = info.Reason,
            ["ms"] = ms,
            ["bytes_out"] = bytesOut,
            ["est_tokens"] = bytesOut / 4,
            ["results"] = info.Results,
            ["truncated"] = info.Truncated,
            ["candidates"] = info.Candidates,
            ["files"] = info.TotalFiles,
            ["note"] = info.Note is { Length: > 500 } n ? n[..500] : info.Note,
            ["error"] = error,
        };
        lock (_lock)
        {
            try
            {
                if (File.Exists(_file) && new FileInfo(_file).Length > RotateBytes)
                    File.Move(_file, _file + ".1", true);
                File.AppendAllText(_file, rec.ToJsonString() + "\n");
            }
            catch (IOException)
            {
            }
        }
    }

    // Copies argument names and numbers/booleans, and hashes (or keeps, if plain) strings.
    private JsonObject Shape(JsonNode? args)
    {
        var shape = new JsonObject();
        if (args is not JsonObject o)
            return shape;
        foreach (var (key, value) in o)
        {
            if (value is JsonValue v && v.TryGetValue<string>(out var s))
                shape[key] = _plain ? s : Hash(s);
            else if (value is JsonArray arr)
                shape[key] = new JsonArray(arr.Select((e, i) => ShapeElement(e, i)).ToArray());
            else
                shape[key] = value?.DeepClone();
        }
        return shape;
    }

    // Keeps the first array element and flags (so command patterns stay readable) and hashes other strings.
    private JsonNode? ShapeElement(JsonNode? e, int index)
    {
        if (e is JsonValue v && v.TryGetValue<string>(out var s))
            return _plain || index == 0 || s.StartsWith('-') ? s : Hash(s);
        return e?.DeepClone();
    }

    // Returns the first 12 hex digits of a SHA-256 hash.
    private static string Hash(string s) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(s)))[..12].ToLowerInvariant();
}
