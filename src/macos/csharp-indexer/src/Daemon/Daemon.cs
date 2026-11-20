// The shared per-user daemon: one engine, one MCP session per connected client, and the status and stop commands.
using System.Collections.Concurrent;
using System.Diagnostics;
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using CSharpIndexer.Mcp;

namespace CSharpIndexer.Daemon;

// Listens on a unix socket; each connection starts with a JSON handshake naming its roots, or a command (status, stop) answered in one reply.
public sealed class DaemonServer(Engine engine, string socketPath, TimeSpan idleExit, string? version = null) : IDisposable
{
    private readonly string _version = version ?? ToolVersion.Text;
    private readonly DateTime _started = DateTime.UtcNow;
    private readonly ConcurrentDictionary<int, ClientInfo> _sessions = new();
    private int _nextSession;
    private readonly Socket _listener = new(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified);
    private readonly CancellationTokenSource _stop = new();
    private int _clients;
    private int _draining;
    private DateTime _lastActive = DateTime.UtcNow;

    private readonly TaskCompletionSource _done = new(TaskCreationOptions.RunContinuationsAsynchronously);

    public Task Done => _done.Task;

    // Binds the socket (refusing if another daemon already answers on it) and starts accepting.
    public void Start()
    {
        _stop.Token.Register(() => _done.TrySetResult());
        if (File.Exists(socketPath))
        {
            if (Proxy.TryConnect(socketPath) is { } live)
            {
                live.Dispose();
                throw new InvalidOperationException("a daemon is already running");
            }
            File.Delete(socketPath);
        }
        _listener.Bind(new UnixDomainSocketEndPoint(socketPath));
        if (!OperatingSystem.IsWindows())
            File.SetUnixFileMode(socketPath, UnixFileMode.UserRead | UnixFileMode.UserWrite);
        _listener.Listen(32);
        _ = Task.Run(AcceptLoop);
        _ = Task.Run(IdleLoop);
    }

    // Accepts clients until stopped.
    private async Task AcceptLoop()
    {
        while (!_stop.IsCancellationRequested)
        {
            Socket client;
            try
            {
                client = await _listener.AcceptAsync(_stop.Token);
            }
            catch (Exception e) when (e is OperationCanceledException or SocketException or ObjectDisposedException)
            {
                return;
            }
            _ = Task.Run(() => Serve(client));
        }
    }

    // Stops the daemon after it has had no clients for the idle period.
    private async Task IdleLoop()
    {
        while (!_stop.IsCancellationRequested)
        {
            await Task.Delay(TimeSpan.FromSeconds(Math.Min(30, Math.Max(0.05, idleExit.TotalSeconds / 4))));
            if (Volatile.Read(ref _clients) == 0 && DateTime.UtcNow - _lastActive > idleExit)
            {
                _stop.Cancel();
                return;
            }
        }
    }

    // Runs one client: handshake, then an MCP session until the client closes.
    private async Task Serve(Socket client)
    {
        Interlocked.Increment(ref _clients);
        var id = Interlocked.Increment(ref _nextSession);
        try
        {
            using var stream = new NetworkStream(client, true);
            using var reader = new StreamReader(stream, new UTF8Encoding(false));
            using var writer = new StreamWriter(stream, new UTF8Encoding(false)) { AutoFlush = false };
            var hello = await Handshake(reader, writer, id);
            if (hello is null)
                return;
            await new McpServer(hello.Value.Roots, engine.Log, hello.Value.Session).RunAsync(reader, writer);
        }
        catch (Exception e) when (e is IOException or SocketException or ObjectDisposedException)
        {
        }
        finally
        {
            _sessions.TryRemove(id, out _);
            _lastActive = DateTime.UtcNow;
            if (Interlocked.Decrement(ref _clients) == 0 && Volatile.Read(ref _draining) == 1)
                _stop.Cancel();
        }
    }

    // Reads the hello line, answers a command, or checks the version, attaches roots, registers the session under id and replies; returns null if the session must end. A newer client makes the daemon drain (finish its clients, refuse new ones, then exit) so the client can start its own; an older one is told to restart its daemon, so two versions never take turns stopping each other.
    private async Task<(List<Root> Roots, string Session)?> Handshake(StreamReader reader, StreamWriter writer, int id)
    {
        var hello = JsonNode.Parse(await reader.ReadLineAsync() ?? "{}");
        if (hello?["command"] is JsonValue command)
        {
            await Reply(writer, Command(command.TryGetValue<string>(out var name) ? name : ""));
            return null;
        }
        var theirs = hello?["version"]?.GetValue<string>() ?? "";
        if (theirs != _version && !ToolVersion.Newer(theirs, _version))
        {
            await Reply(writer, new JsonObject { ["ok"] = false, ["restart"] = false, ["version"] = _version, ["error"] = $"the indexer daemon is version {_version}, newer than this client's {theirs}: restart it with `zazzle csharp-indexer restart`" });
            return null;
        }
        if (theirs != _version)
            Volatile.Write(ref _draining, 1);
        if (Volatile.Read(ref _draining) == 1)
        {
            await Reply(writer, new JsonObject { ["ok"] = false, ["restart"] = true, ["error"] = "the indexer daemon is making way for a newer version" });
            if (Volatile.Read(ref _clients) == 1)
                _stop.Cancel();
            return null;
        }
        try
        {
            var roots = (hello?["roots"]?.AsArray() ?? []).Select(r => Root.Parse(r!.GetValue<string>(), engine)).ToList();
            var session = hello?["session"]?.GetValue<string>() ?? "";
            _sessions[id] = new ClientInfo(session, roots.Select(r => r.Name).ToArray(), DateTime.UtcNow);
            await Reply(writer, new JsonObject { ["ok"] = true });
            return (roots, session);
        }
        catch (Exception e) when (e is ArgumentException or InvalidOperationException or IOException)
        {
            await Reply(writer, new JsonObject { ["ok"] = false, ["error"] = e.Message });
            return null;
        }
    }

    // Answers a command hello: status describes the daemon; stop also makes it drain, so it exits once its clients have finished.
    private JsonObject Command(string name)
    {
        if (name == "stop")
            Volatile.Write(ref _draining, 1);
        if (name is "status" or "stop")
            return new JsonObject { ["ok"] = true, ["status"] = Status() };
        return new JsonObject { ["ok"] = false, ["restart"] = false, ["version"] = _version, ["error"] = $"unknown command: {name}" };
    }

    // Describes the daemon: process, version, uptime, socket, state folder, memory, connected clients and the roots it indexes.
    public JsonObject Status()
    {
        var now = DateTime.UtcNow;
        using var me = Process.GetCurrentProcess();
        var clients = _sessions.Values.OrderBy(c => c.Since).Select(c => (JsonNode)new JsonObject
        {
            ["session"] = c.Session,
            ["roots"] = new JsonArray(c.Roots.Select(r => (JsonNode)r).ToArray()),
            ["connected_at"] = c.Since.ToString("o"),
            ["connected_seconds"] = (long)(now - c.Since).TotalSeconds,
        });
        var roots = engine.Indexes().Select(i => (JsonNode)new JsonObject
        {
            ["path"] = i.Path,
            ["files"] = i.FileCount,
            ["ready"] = i.Ready.IsCompleted,
            ["indexed_at"] = i.IndexedAt?.ToString("o"),
            ["changed_at"] = i.ChangedAt?.ToString("o"),
            ["age_seconds"] = i.UpdatedAt is { } u ? (long)(now - u).TotalSeconds : null,
        });
        return new JsonObject
        {
            ["pid"] = Environment.ProcessId,
            ["version"] = _version,
            ["started"] = _started.ToString("o"),
            ["uptime_seconds"] = (long)(now - _started).TotalSeconds,
            ["socket"] = socketPath,
            ["state_dir"] = engine.StateDir,
            ["draining"] = Volatile.Read(ref _draining) == 1,
            ["memory"] = new JsonObject { ["working_set_bytes"] = me.WorkingSet64, ["managed_heap_bytes"] = GC.GetTotalMemory(false) },
            ["clients"] = new JsonArray(clients.ToArray()),
            ["roots"] = new JsonArray(roots.ToArray()),
        };
    }

    // Writes one JSON line and flushes.
    private static async Task Reply(StreamWriter w, JsonNode node)
    {
        await w.WriteLineAsync(node.ToJsonString());
        await w.FlushAsync();
    }

    // Stops accepting and closes the socket file.
    public void Dispose()
    {
        _stop.Cancel();
        _listener.Dispose();
        try
        {
            File.Delete(socketPath);
        }
        catch (IOException)
        {
        }
    }
}

// One connected MCP session: the session id its client gave, the names of its roots and when it connected.
public sealed record ClientInfo(string Session, string[] Roots, DateTime Since);
