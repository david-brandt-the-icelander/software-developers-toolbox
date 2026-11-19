// Tests for the shared daemon and its handshake.
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using CSharpIndexer.Daemon;
using Xunit;

namespace CSharpIndexer.Tests;

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
    private static (DaemonServer daemon, string sock) StartDaemon(TempRepo repo, TimeSpan? idle = null, string? version = null)
    {
        var sock = Path.Combine(Directory.CreateTempSubdirectory("d").FullName, "d.sock");
        var daemon = new DaemonServer(repo.Engine, sock, idle ?? TimeSpan.FromMinutes(5), version);
        daemon.Start();
        return (daemon, sock);
    }

    [Fact]
    // Two clients share one engine and both get answers from the same index.
    public async Task test_two_clients_share_engine()
    {
        using var repo = new TempRepo(new() { ["a.cs"] = "class Needle {}\n" });
        var (daemon, sock) = StartDaemon(repo);
        using var _ = daemon;
        var answers = Enumerable.Range(0, 2).Select(_ => Task.Run(() =>
        {
            var socket = Proxy.TryConnect(sock)!;
            Assert.True(Proxy.Hello(socket, ["r=" + repo.Dir], "client-session")["ok"]!.GetValue<bool>());
            using var s = new NetworkStream(socket, true);
            var r = Rpc(s, "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"find_symbol\",\"arguments\":{\"name\":\"needle\"}}}");
            return r["result"]!["content"]![0]!["text"]!.GetValue<string>();
        })).ToArray();
        foreach (var a in answers)
            Assert.StartsWith("r:a.cs:1 class Needle", await a);
        var records = File.ReadLines(Path.Combine(repo.Engine.StateDir, "usage.jsonl")).Select(line => JsonNode.Parse(line)).ToList();
        Assert.Equal(2, records.Count);
        Assert.All(records, r => Assert.Equal("client-session", r!["session"]!.GetValue<string>()));
    }

    [Fact]
    // An older client is told to restart its daemon and the daemon stays; a newer one makes the daemon drain: busy clients finish, new ones are refused, then it exits.
    public async Task test_version_mismatch()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "x\n" });
        var (daemon, sock) = StartDaemon(repo, version: "1.2.17");
        using var _ = daemon;
        JsonNode Hello(string version)
        {
            using var s = new NetworkStream(Proxy.TryConnect(sock)!, true);
            return Rpc(s, $"{{\"version\":\"{version}\",\"roots\":[]}}");
        }
        var old = Hello("0.0.1");
        Assert.False(old["restart"]!.GetValue<bool>());
        Assert.Contains("newer than this client's 0.0.1", old["error"]!.GetValue<string>());
        Assert.Contains("version 1.2.17", old["error"]!.GetValue<string>());
        Assert.Contains("restart it with `zazzle csharp-indexer restart`", old["error"]!.GetValue<string>());
        Assert.Equal("1.2.17", old["version"]!.GetValue<string>());
        Assert.False(daemon.Done.IsCompleted);
        var busy = new NetworkStream(Proxy.TryConnect(sock)!, true);
        Rpc(busy, "{\"version\":\"1.2.17\",\"roots\":[]}");
        var newer = Hello("1.2.18");
        Assert.True(newer["restart"]!.GetValue<bool>());
        Assert.True(Hello("1.2.17")["restart"]!.GetValue<bool>());
        Assert.False(daemon.Done.IsCompleted);
        busy.Dispose();
        Assert.Same(daemon.Done, await Task.WhenAny(daemon.Done, Task.Delay(5000)));
    }

    [Fact]
    // The status command describes the daemon, its clients and roots without opening a session; stop drains it like a newer client, and an unknown command is refused.
    public async Task test_status_and_stop_commands()
    {
        using var repo = new TempRepo(new() { ["a.cs"] = "class A {}\n", ["b.txt"] = "x\n" });
        var (daemon, sock) = StartDaemon(repo, version: "1.2.17");
        using var _ = daemon;
        JsonNode Command(string json)
        {
            using var s = new NetworkStream(Proxy.TryConnect(sock)!, true);
            return Rpc(s, json);
        }
        var busy = new NetworkStream(Proxy.TryConnect(sock)!, true);
        Assert.True(Rpc(busy, $"{{\"version\":\"1.2.17\",\"roots\":[\"r={repo.Dir}\"],\"session\":\"s1\"}}")["ok"]!.GetValue<bool>());
        repo.Engine.Indexes()[0].WaitReady(30000);
        var status = Command("{\"command\":\"status\"}")["status"]!;
        Assert.Equal(Environment.ProcessId, status["pid"]!.GetValue<int>());
        Assert.Equal("1.2.17", status["version"]!.GetValue<string>());
        Assert.Equal(sock, status["socket"]!.GetValue<string>());
        Assert.Equal(repo.Engine.StateDir, status["state_dir"]!.GetValue<string>());
        Assert.False(status["draining"]!.GetValue<bool>());
        Assert.Equal("s1", status["clients"]![0]!["session"]!.GetValue<string>());
        Assert.Single(status["clients"]!.AsArray());
        var root = status["roots"]![0]!;
        Assert.Equal(2, root["files"]!.GetValue<int>());
        Assert.True(root["ready"]!.GetValue<bool>());
        Assert.True(root["age_seconds"]!.GetValue<long>() >= 0);
        var unknown = Command("{\"command\":\"dance\"}");
        Assert.False(unknown["ok"]!.GetValue<bool>());
        Assert.Contains("unknown command: dance", unknown["error"]!.GetValue<string>());
        var stop = Command("{\"command\":\"stop\"}")["status"]!;
        Assert.True(stop["draining"]!.GetValue<bool>());
        Assert.Single(stop["clients"]!.AsArray());
        Assert.True(Command("{\"version\":\"1.2.17\",\"roots\":[]}")["restart"]!.GetValue<bool>());
        Assert.False(daemon.Done.IsCompleted);
        busy.Dispose();
        Assert.Same(daemon.Done, await Task.WhenAny(daemon.Done, Task.Delay(5000)));
    }

    [Fact]
    // Versions compare by major.minor.patch, and a development build is newest.
    public void test_newer()
    {
        Assert.True(ToolVersion.Newer("1.2.18", "1.2.17"));
        Assert.False(ToolVersion.Newer("1.2.9", "1.2.17"));
        Assert.False(ToolVersion.Newer("1.2.17", "1.2.17"));
        Assert.True(ToolVersion.Newer("2.0", "1.9.9"));
        Assert.True(ToolVersion.Newer("0.0.0-dev", "9.9.9"));
        Assert.False(ToolVersion.Newer("9.9.9", "0.0.0-dev"));
        Assert.False(ToolVersion.Newer("x", "1.0.0"));
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
    // The handshake reply is read as UTF-8, so an error naming a non-ASCII path arrives as sent.
    public void test_hello_reply_utf8()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "x\n" });
        var (daemon, sock) = StartDaemon(repo);
        using var _ = daemon;
        var missing = Path.Combine(Path.GetTempPath(), "naïve-ö-" + Guid.NewGuid().ToString("N"));
        var reply = Proxy.Hello(Proxy.TryConnect(sock)!, [missing]);
        Assert.StartsWith("not a directory: ", reply["error"]!.GetValue<string>());
        Assert.EndsWith(Path.GetFileName(missing), reply["error"]!.GetValue<string>());
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
