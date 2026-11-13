// The shared per-user daemon: one engine, one MCP session per connected client.
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using ZazzleIndexer.Mcp;

namespace ZazzleIndexer.Daemon;

// Listens on a unix socket; each connection starts with a JSON handshake naming its roots.
public sealed class DaemonServer(Engine engine, string socketPath, TimeSpan idleExit) : IDisposable
{
    private readonly Socket _listener = new(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified);
    private readonly CancellationTokenSource _stop = new();
    private int _clients;
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
        try
        {
            using var stream = new NetworkStream(client, true);
            using var reader = new StreamReader(stream, new UTF8Encoding(false));
            using var writer = new StreamWriter(stream, new UTF8Encoding(false)) { AutoFlush = false };
            var roots = await Handshake(reader, writer);
            if (roots is null)
                return;
            await new McpServer(roots, engine.Log).RunAsync(reader, writer);
        }
        catch (Exception e) when (e is IOException or SocketException or ObjectDisposedException)
        {
        }
        finally
        {
            _lastActive = DateTime.UtcNow;
            Interlocked.Decrement(ref _clients);
        }
    }

    // Reads the hello line, checks the version, attaches roots and replies; returns null if the session must end.
    private async Task<List<Root>?> Handshake(StreamReader reader, StreamWriter writer)
    {
        var hello = JsonNode.Parse(await reader.ReadLineAsync() ?? "{}");
        if (hello?["version"]?.GetValue<string>() != ToolVersion.Text)
        {
            var idle = Volatile.Read(ref _clients) == 1;
            await Reply(writer, new JsonObject { ["ok"] = false, ["restart"] = idle, ["error"] = "daemon version differs" });
            if (idle)
                _stop.Cancel();
            return null;
        }
        try
        {
            var roots = hello["roots"]!.AsArray().Select(r => Root.Parse(r!.GetValue<string>(), engine)).ToList();
            await Reply(writer, new JsonObject { ["ok"] = true });
            return roots;
        }
        catch (Exception e) when (e is ArgumentException or InvalidOperationException or IOException)
        {
            await Reply(writer, new JsonObject { ["ok"] = false, ["error"] = e.Message });
            return null;
        }
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
