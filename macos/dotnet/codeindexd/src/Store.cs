// SQLite store: roots, per-path stat keys and content-addressed blob analysis.
using Microsoft.Data.Sqlite;
using CodeIndexd.Index;

namespace CodeIndexd;

// Thread-safe store; every operation opens a pooled connection and writes are serialised.
public sealed class Store
{
    public const int ExtractorVersion = 1;
    private readonly string _cs;
    private readonly object _write = new();

    // Opens (creating if needed) the database file and drops blobs from older extractors.
    public Store(string file)
    {
        _cs = new SqliteConnectionStringBuilder { DataSource = file, Mode = SqliteOpenMode.ReadWriteCreate }.ToString();
        lock (_write)
        {
            using var c = Open();
            Exec(c, "PRAGMA journal_mode=WAL");
            Exec(c, """
                CREATE TABLE IF NOT EXISTS root(id INTEGER PRIMARY KEY, path TEXT UNIQUE, event_id INTEGER, full_scan INTEGER);
                CREATE TABLE IF NOT EXISTS blob(id INTEGER PRIMARY KEY, oid TEXT UNIQUE, ver INTEGER, size INTEGER, binary INTEGER,
                    lang TEXT, lines INTEGER, bloom BLOB, digest TEXT, symbols TEXT, idents TEXT);
                CREATE TABLE IF NOT EXISTS file(root_id INTEGER, path TEXT, size INTEGER, mtime INTEGER, birth INTEGER, blob_id INTEGER,
                    PRIMARY KEY(root_id, path)) WITHOUT ROWID;
                """);
            Exec(c, $"DELETE FROM blob WHERE ver <> {ExtractorVersion}");
        }
    }

    // Opens a connection with a busy timeout.
    private SqliteConnection Open()
    {
        var c = new SqliteConnection(_cs);
        c.Open();
        Exec(c, "PRAGMA busy_timeout=10000; PRAGMA synchronous=NORMAL");
        return c;
    }

    // Runs one statement with no parameters.
    private static void Exec(SqliteConnection c, string sql)
    {
        using var cmd = c.CreateCommand();
        cmd.CommandText = sql;
        cmd.ExecuteNonQuery();
    }

    // Runs a write statement under the write lock with named parameters.
    private void Write(string sql, params (string, object?)[] ps)
    {
        lock (_write)
        {
            using var c = Open();
            using var cmd = Bind(c, sql, ps);
            cmd.ExecuteNonQuery();
        }
    }

    // Builds a command with named parameters.
    private static SqliteCommand Bind(SqliteConnection c, string sql, (string, object?)[] ps)
    {
        var cmd = c.CreateCommand();
        cmd.CommandText = sql;
        foreach (var (n, v) in ps)
            cmd.Parameters.AddWithValue(n, v ?? DBNull.Value);
        return cmd;
    }

    // Returns the id of a root path, creating it on first use.
    public int RootId(string path)
    {
        Write("INSERT OR IGNORE INTO root(path) VALUES(@p)", ("@p", path));
        return (int)(long)Scalar("SELECT id FROM root WHERE path=@p", ("@p", path))!;
    }

    // Runs a query returning one value.
    private object? Scalar(string sql, params (string, object?)[] ps)
    {
        using var c = Open();
        using var cmd = Bind(c, sql, ps);
        var v = cmd.ExecuteScalar();
        return v is DBNull ? null : v;
    }

    // Reads the stored FSEvents id and last full scan time (UTC ticks) of a root.
    public (long? eventId, long? fullScan) RootState(int id)
    {
        using var c = Open();
        using var cmd = Bind(c, "SELECT event_id, full_scan FROM root WHERE id=@i", [("@i", id)]);
        using var r = cmd.ExecuteReader();
        r.Read();
        return (r.IsDBNull(0) ? null : r.GetInt64(0), r.IsDBNull(1) ? null : r.GetInt64(1));
    }

    // Stores the FSEvents id of a root.
    public void SetEventId(int id, long eventId) => Write("UPDATE root SET event_id=@e WHERE id=@i", ("@e", eventId), ("@i", id));

    // Stores the time of the last full scan of a root.
    public void SetFullScan(int id, long ticks) => Write("UPDATE root SET full_scan=@t WHERE id=@i", ("@t", ticks), ("@i", id));

    // Loads a root's file records; files whose blob is gone get BlobId -1 so they are re-analysed.
    public List<FileEntry> LoadFiles(int rootId)
    {
        var list = new List<FileEntry>();
        using var c = Open();
        using var cmd = Bind(c, "SELECT f.path, f.size, f.mtime, f.birth, f.blob_id, b.id FROM file f LEFT JOIN blob b ON b.id=f.blob_id WHERE f.root_id=@r", [("@r", rootId)]);
        using var r = cmd.ExecuteReader();
        while (r.Read())
        {
            var blob = r.GetInt32(4);
            list.Add(new FileEntry(r.GetString(0), r.GetInt64(1), r.GetInt64(2), r.GetInt64(3), blob == 0 || !r.IsDBNull(5) ? blob : -1));
        }
        return list;
    }

    // Inserts or replaces a file record.
    public void UpsertFile(int rootId, FileEntry e) => Write(
        "INSERT OR REPLACE INTO file(root_id, path, size, mtime, birth, blob_id) VALUES(@r,@p,@s,@m,@b,@x)",
        ("@r", rootId), ("@p", e.Path), ("@s", e.Size), ("@m", e.Mtime), ("@b", e.Birth), ("@x", e.BlobId));

    // Deletes a file record.
    public void DeleteFile(int rootId, string path) => Write("DELETE FROM file WHERE root_id=@r AND path=@p", ("@r", rootId), ("@p", path));

    // Inserts an analysed blob and returns its id.
    public int InsertBlob(string oid, long size, bool binary, string lang, int lines, byte[] bloom, string digest, string? symbols, string? idents)
    {
        lock (_write)
        {
            using var c = Open();
            using var cmd = Bind(c, "INSERT OR IGNORE INTO blob(oid, ver, size, binary, lang, lines, bloom, digest, symbols, idents) VALUES(@o,@v,@s,@bi,@l,@n,@bl,@d,@sy,@id)",
                [("@o", oid), ("@v", ExtractorVersion), ("@s", size), ("@bi", binary ? 1 : 0), ("@l", lang), ("@n", lines), ("@bl", bloom), ("@d", digest), ("@sy", symbols), ("@id", idents)]);
            cmd.ExecuteNonQuery();
            using var q = Bind(c, "SELECT id FROM blob WHERE oid=@o", [("@o", oid)]);
            return (int)(long)q.ExecuteScalar()!;
        }
    }

    // Loads the in-memory facts of every blob.
    public List<BlobMeta> LoadBlobs()
    {
        var list = new List<BlobMeta>();
        using var c = Open();
        using var cmd = Bind(c, "SELECT id, oid, size, binary, lang, lines, bloom, digest FROM blob", []);
        using var r = cmd.ExecuteReader();
        while (r.Read())
            list.Add(new BlobMeta(r.GetInt32(0), r.GetString(1), r.GetInt64(2), r.GetInt32(3) == 1, r.GetString(4), r.GetInt32(5), (byte[])r["bloom"], r.GetString(7)));
        return list;
    }

    // Reads the symbols JSON of a blob.
    public string? Symbols(int blobId) => Scalar("SELECT symbols FROM blob WHERE id=@i", ("@i", blobId)) as string;

    // Reads the identifier-occurrence JSON of a blob.
    public string? Idents(int blobId) => Scalar("SELECT idents FROM blob WHERE id=@i", ("@i", blobId)) as string;

    // Deletes blobs no file refers to and returns how many were removed.
    public int CollectGarbage()
    {
        lock (_write)
        {
            using var c = Open();
            using var cmd = Bind(c, "DELETE FROM blob WHERE id NOT IN (SELECT DISTINCT blob_id FROM file)", []);
            return cmd.ExecuteNonQuery();
        }
    }
}
