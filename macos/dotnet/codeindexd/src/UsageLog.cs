// Append-only JSONL record of every tool call, kept locally to guide later versions.
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;

namespace CodeIndexd;

// What one tool call did, filled in by the tool.
public sealed class CallInfo
{
    public string Path = "index";
    public string? Reason;
    public int Results;
    public bool Truncated;
}

// Thread-safe JSONL writer; a null path disables logging.
public sealed class UsageLog
{
    private const long RotateBytes = 20 * 1024 * 1024;
    private readonly string? _file;
    private readonly bool _plain = Environment.GetEnvironmentVariable("CODEINDEXD_LOG_PLAIN") == "1";
    private readonly object _lock = new();

    // Creates a log writing to the given file.
    public UsageLog(string? file) => _file = file;

    // Opens the log in the state directory (CODEINDEXD_STATE_DIR overrides it).
    public static UsageLog Open()
    {
        var dir = Environment.GetEnvironmentVariable("CODEINDEXD_STATE_DIR") ?? DefaultDir();
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

    // Picks the platform state directory.
    private static string DefaultDir()
    {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        if (OperatingSystem.IsMacOS())
            return System.IO.Path.Combine(home, "Library", "Application Support", "codeindexd");
        var xdg = Environment.GetEnvironmentVariable("XDG_STATE_HOME");
        return System.IO.Path.Combine(string.IsNullOrEmpty(xdg) ? System.IO.Path.Combine(home, ".local", "state") : xdg, "codeindexd");
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
            else
                shape[key] = value?.DeepClone();
        }
        return shape;
    }

    // Returns the first 12 hex digits of a SHA-256 hash.
    private static string Hash(string s) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(s)))[..12].ToLowerInvariant();
}
