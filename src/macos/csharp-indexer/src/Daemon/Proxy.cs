// Client side of the daemon protocol: starts the daemon if needed and pipes stdio to it.
using System.Diagnostics;
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;

namespace CSharpIndexer.Daemon;

// Static helpers for connecting and proxying.
public static class Proxy
{
    // Connects to the daemon socket, or returns null if nothing answers.
    public static Socket? TryConnect(string socketPath)
    {
        if (!File.Exists(socketPath))
            return null;
        var s = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified);
        try
        {
            s.Connect(new UnixDomainSocketEndPoint(socketPath));
            return s;
        }
        catch (SocketException)
        {
            s.Dispose();
            return null;
        }
    }

    // Returns the socket path inside the state directory.
    public static string SocketPath(string stateDir) => Path.Combine(stateDir, "daemon.sock");

    // Starts a detached daemon process from an executable (default: this one), on a given state folder when one is named.
    public static void SpawnDaemon(string? exe = null, string? stateDir = null)
    {
        exe ??= Environment.ProcessPath!;
        var psi = OperatingSystem.IsWindows()
            ? new ProcessStartInfo(exe, "daemon") { UseShellExecute = false, CreateNoWindow = true }
            : new ProcessStartInfo("/bin/sh") { UseShellExecute = false, ArgumentList = { "-c", "nohup \"$0\" daemon >/dev/null 2>&1 </dev/null &", exe } };
        if (stateDir is not null)
            psi.Environment["ZAZZLE_INDEXER_STATE_DIR"] = stateDir;
        Process.Start(psi)?.WaitForExit(2000);
    }

    // Connects (starting the daemon if needed), performs the handshake and returns the ready socket, or null if no daemon works.
    public static Socket? Connect(string stateDir, IEnumerable<string> rootSpecs, out string? error)
    {
        error = null;
        var path = SocketPath(stateDir);
        for (var attempt = 0; attempt < 3; attempt++)
        {
            var socket = TryConnect(path);
            if (socket is null)
            {
                SpawnDaemon();
                socket = WaitConnect(path);
            }
            if (socket is null)
            {
                error = "daemon did not start";
                return null;
            }
            var reply = Hello(socket, rootSpecs);
            if (reply["ok"]?.GetValue<bool>() == true)
                return socket;
            socket.Dispose();
            error = reply["error"]?.GetValue<string>();
            if (reply["restart"]?.GetValue<bool>() != true)
                return null;
            Thread.Sleep(500);
        }
        return null;
    }

    // Polls the socket for up to five seconds.
    private static Socket? WaitConnect(string path)
    {
        var end = DateTime.UtcNow.AddSeconds(5);
        while (DateTime.UtcNow < end)
        {
            if (TryConnect(path) is { } s)
                return s;
            Thread.Sleep(50);
        }
        return null;
    }

    // Sends the hello line and reads the daemon's reply line.
    public static JsonNode Hello(Socket socket, IEnumerable<string> rootSpecs, string? session = null) =>
        Exchange(socket, new JsonObject { ["version"] = ToolVersion.Text, ["roots"] = new JsonArray(rootSpecs.Select(r => (JsonNode)r).ToArray()), ["session"] = session });

    // Sends one JSON line and reads the reply line; throws IOException when the daemon closes the connection without one.
    public static JsonNode Exchange(Socket socket, JsonObject message)
    {
        var stream = new NetworkStream(socket, false);
        var bytes = Encoding.UTF8.GetBytes(message.ToJsonString() + "\n");
        stream.Write(bytes, 0, bytes.Length);
        var line = new MemoryStream();
        int b;
        while ((b = stream.ReadByte()) is not (-1 or '\n'))
            line.WriteByte((byte)b);
        if (line.Length == 0)
            throw new IOException("the daemon closed the connection without a reply");
        return JsonNode.Parse(line.ToArray()) ?? new JsonObject();
    }

    // Pipes stdin to the socket and the socket to stdout until the daemon closes the connection.
    public static async Task Pump(Socket socket)
    {
        using var stream = new NetworkStream(socket, true);
        var up = Task.Run(async () =>
        {
            await Console.OpenStandardInput().CopyToAsync(stream);
            socket.Shutdown(SocketShutdown.Send);
        });
        await stream.CopyToAsync(Console.OpenStandardOutput());
        await Task.WhenAny(up, Task.Delay(100));
    }
}
