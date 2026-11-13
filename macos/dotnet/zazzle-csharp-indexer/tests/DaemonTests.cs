// Tests for the shared daemon and its handshake.
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using ZazzleIndexer.Daemon;
using Xunit;

namespace ZazzleIndexer.Tests;

// Runs the daemon in-process on a temporary unix socket.
public class DaemonTests
{
    // Sends one JSON-RPC request line over an open session and reads the reply line.
    private static JsonNode Rpc(NetworkStream s, string json)
    {
        s.Write(Encoding.UTF8.GetBytes(json + "\n"));
        var sb = new StringBuilder();
        int b;
        while ((b = s.ReadByte()) is not (-1 or '\n'))
            sb.Append((char)b);
        return JsonNode.Parse(sb.ToString())!;
    }

    // Starts a daemon for a repo and returns it with its socket path.
    private static (DaemonServer daemon, string sock) StartDaemon(TempRepo repo, TimeSpan? idle = null)
    {
        var sock = Path.Combine(Directory.CreateTempSubdirectory("d").FullName, "d.sock");
        var daemon = new DaemonServer(repo.Engine, sock, idle ?? TimeSpan.FromMinutes(5));
        daemon.Start();
        return (daemon, sock);
    }

    [Fact]
    // Two clients share one engine and both get answers from the same index.
    public async Task test_two_clients_share_engine()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "shared needle\n" });
        var (daemon, sock) = StartDaemon(repo);
        using var _ = daemon;
        var answers = Enumerable.Range(0, 2).Select(_ => Task.Run(() =>
        {
            var socket = Proxy.TryConnect(sock)!;
            Assert.True(Proxy.Hello(socket, ["r=" + repo.Dir])["ok"]!.GetValue<bool>());
            using var s = new NetworkStream(socket, true);
            var r = Rpc(s, "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"pattern\":\"needle\"}}}");
            return r["result"]!["content"]![0]!["text"]!.GetValue<string>();
        })).ToArray();
        foreach (var a in answers)
            Assert.Contains("r:a.txt\n1:shared needle", await a);
    }

    [Fact]
    // A client with a different version is told to restart when it is the only one connected.
    public async Task test_version_mismatch_requests_restart()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "x\n" });
        var (daemon, sock) = StartDaemon(repo);
        using var _ = daemon;
        var socket = Proxy.TryConnect(sock)!;
        using var s = new NetworkStream(socket, true);
        var reply = Rpc(s, "{\"version\":\"0.0.0\",\"roots\":[]}");
        Assert.False(reply["ok"]!.GetValue<bool>());
        Assert.True(reply["restart"]!.GetValue<bool>());
        Assert.Same(daemon.Done, await Task.WhenAny(daemon.Done, Task.Delay(5000)));
    }

    [Fact]
    // A root that is not a git work tree is rejected in the handshake.
    public void test_bad_root_rejected()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "x\n" });
        var (daemon, sock) = StartDaemon(repo);
        using var _ = daemon;
        var socket = Proxy.TryConnect(sock)!;
        var reply = Proxy.Hello(socket, [Path.GetTempPath()]);
        Assert.False(reply["ok"]!.GetValue<bool>());
        Assert.Contains("not inside a git work tree", reply["error"]!.GetValue<string>());
    }

    [Fact]
    // The daemon exits after the idle period with no clients.
    public async Task test_idle_exit()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "x\n" });
        var (daemon, _) = StartDaemon(repo, TimeSpan.FromMilliseconds(200));
        using var _d = daemon;
        Assert.Same(daemon.Done, await Task.WhenAny(daemon.Done, Task.Delay(10000)));
    }

    [Fact]
    // A second daemon on the same socket refuses to start.
    public void test_second_daemon_refused()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "x\n" });
        var (daemon, sock) = StartDaemon(repo);
        using var _ = daemon;
        using var second = new DaemonServer(repo.Engine, sock, TimeSpan.FromMinutes(1));
        Assert.Throws<InvalidOperationException>(() => second.Start());
    }
}
