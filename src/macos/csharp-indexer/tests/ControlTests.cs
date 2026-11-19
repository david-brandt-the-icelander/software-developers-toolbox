// Tests for status, stop and restart against real daemon processes, an older daemon's protocol, and the help text.
using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using CSharpIndexer.Daemon;
using Xunit;

namespace CSharpIndexer.Tests;

// Runs the built csharp-indexer executable as the daemon on a temporary state folder.
public class ControlTests
{
    // Finds the executable the build put in src/bin (self-contained, so under a runtime folder such as linux-x64).
    internal static string IndexerExe()
    {
        var bin = new DirectoryInfo(AppContext.BaseDirectory.TrimEnd('/'));
        var dir = Path.GetFullPath(Path.Combine(bin.FullName, "..", "..", "..", "..", "src", "bin", bin.Parent!.Name, bin.Name));
        return Directory.GetFiles(dir, "csharp-indexer", SearchOption.AllDirectories).OrderByDescending(File.GetLastWriteTimeUtc).First();
    }

    // Runs a command with output captured and returns its exit code and output.
    private static (int Code, string Text) Run(Func<TextWriter, int> command)
    {
        var w = new StringWriter();
        return (command(w), w.ToString());
    }

    [Fact]
    // With no daemon, status says so in text and JSON and exits 3, and stop has nothing to do.
    public void test_status_not_running()
    {
        var state = Directory.CreateTempSubdirectory("st").FullName;
        var (code, text) = Run(w => Control.Status(state, false, w));
        Assert.Equal(3, code);
        Assert.StartsWith("csharp-indexer daemon: not running", text);
        Assert.Contains(Path.Combine(state, "daemon.sock"), text);
        var (jcode, json) = Run(w => Control.Status(state, true, w));
        Assert.Equal(3, jcode);
        var j = JsonNode.Parse(json)!;
        Assert.Equal("csharp-indexer", j["tool"]!.GetValue<string>());
        Assert.Equal(ToolVersion.Text, j["version"]!.GetValue<string>());
        Assert.False(j["running"]!.GetValue<bool>());
        Assert.False(j["ok"]!.GetValue<bool>());
        Assert.Equal((0, "no csharp-indexer daemon was running\n"), Run(w => Control.Stop(state, TimeSpan.FromSeconds(1), w)));
        Directory.Delete(state, true);
    }

    [Fact]
    // restart starts a daemon when none runs; status then lists its client and root; restart kills a daemon whose client stays past the timeout; stop stops the fresh one gracefully.
    public void test_restart_status_stop()
    {
        Environment.SetEnvironmentVariable("ZAZZLE_INDEXER_IDLE_MINUTES", "2");
        using var repo = new TempRepo(new() { ["a.cs"] = "class A {}\n" });
        var state = Directory.CreateTempSubdirectory("st").FullName;
        var exe = IndexerExe();
        try
        {
            var (code, text) = Run(w => Control.Restart(state, exe, TimeSpan.FromSeconds(5), w));
            Assert.Equal(0, code);
            Assert.Contains("no csharp-indexer daemon was running", text);
            Assert.Matches(@"started csharp-indexer daemon \S+ \(pid \d+\)", text);

            var client = Proxy.Connect(state, ["r=" + repo.Dir], out var error);
            Assert.True(client is not null, error);
            var (scode, json) = Run(w => Control.Status(state, true, w));
            Assert.Equal(0, scode);
            var j = JsonNode.Parse(json)!;
            Assert.True(j["running"]!.GetValue<bool>());
            Assert.True(j["ok"]!.GetValue<bool>());
            Assert.Equal(ToolVersion.Text, j["daemon_version"]!.GetValue<string>());
            var pid = j["pid"]!.GetValue<int>();
            Assert.Equal("r", j["clients"]![0]!["roots"]![0]!.GetValue<string>());
            Assert.Equal(Native.RealPath(repo.Dir), j["roots"]![0]!["path"]!.GetValue<string>());
            Assert.True(j["memory"]!["working_set_bytes"]!.GetValue<long>() > 0);
            var (_, human) = Run(w => Control.Status(state, false, w));
            Assert.Contains($"running (pid {pid}, version {ToolVersion.Text}, up ", human);
            Assert.Contains("clients: 1", human);
            Assert.Contains("roots: 1", human);

            var (rcode, restarted) = Run(w => Control.Restart(state, exe, TimeSpan.FromSeconds(1), w));
            Assert.Equal(0, rcode);
            Assert.Contains($"waiting up to 1 s for 1 client(s) of the csharp-indexer daemon {ToolVersion.Text} (pid {pid}) to finish", restarted);
            Assert.Contains($"killed csharp-indexer daemon {ToolVersion.Text} (pid {pid})", restarted);
            Assert.DoesNotContain($"(pid {pid})", restarted.Split('\n').Last(l => l.StartsWith("started")));
            client!.Dispose();

            var (stop, stopped) = Run(w => Control.Stop(state, TimeSpan.FromSeconds(10), w));
            Assert.Equal(0, stop);
            Assert.StartsWith($"stopped csharp-indexer daemon {ToolVersion.Text} (pid ", stopped);
            Assert.Equal(3, Run(w => Control.Status(state, false, w)).Code);
        }
        finally
        {
            Control.Stop(state, TimeSpan.Zero, TextWriter.Null);
            Directory.Delete(state, true);
        }
    }

    [Fact]
    // An older daemon, which has no commands, is reported by the version in its refusal, and stopped by the drain a newer client causes.
    public async Task test_older_daemon()
    {
        var state = Directory.CreateTempSubdirectory("st").FullName;
        var sock = Path.Combine(state, "daemon.sock");
        using var listener = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified);
        listener.Bind(new UnixDomainSocketEndPoint(sock));
        listener.Listen(8);
        var old = Task.Run(() => OldDaemon(listener, sock));
        var probe = Control.Query(state);
        Assert.Equal(new Probe(true, null, "1.0.0", null), probe);
        var (code, text) = Run(w => Control.Status(state, false, w));
        Assert.Equal(0, code);
        Assert.StartsWith("csharp-indexer daemon: running, version 1.0.0; details need a newer daemon", text);
        Assert.Equal("1.0.0", JsonNode.Parse(Run(w => Control.Status(state, true, w)).Text)!["daemon_version"]!.GetValue<string>());
        var (stop, stopped) = Run(w => Control.Stop(state, TimeSpan.FromSeconds(5), w));
        Assert.Equal(0, stop);
        Assert.Contains("asked the csharp-indexer daemon 1.0.0, which predates the stop command, to finish its clients and exit", stopped);
        Assert.EndsWith("stopped csharp-indexer daemon 1.0.0\n", stopped);
        Assert.Same(old, await Task.WhenAny(old, Task.Delay(5000)));
        Directory.Delete(state, true);
    }

    // Answers hellos the way a daemon from before the commands did: too old a client is refused with the daemon's version; a newer one makes it exit.
    private static void OldDaemon(Socket listener, string sock)
    {
        while (true)
        {
            using var client = listener.Accept();
            using var stream = new NetworkStream(client);
            using var reader = new StreamReader(stream);
            var hello = JsonNode.Parse(reader.ReadLine() ?? "{}");
            var theirs = hello?["version"]?.GetValue<string>() ?? "";
            var newer = ToolVersion.Newer(theirs, "1.0.0");
            var reply = newer
                ? new JsonObject { ["ok"] = false, ["restart"] = true, ["error"] = "the indexer daemon is making way for a newer version" }
                : new JsonObject { ["ok"] = false, ["restart"] = false, ["error"] = $"the indexer daemon is version 1.0.0, newer than this client's {theirs}: restart daemon (zazzle daemon uninstall && zazzle daemon install, or quit the apps)" };
            stream.Write(Encoding.UTF8.GetBytes(reply.ToJsonString() + "\n"));
            if (!newer)
                continue;
            listener.Close();
            File.Delete(sock);
            return;
        }
    }

    [Fact]
    // help lists every command on stdout, --version prints the version, and an unknown command exits 2.
    public void test_help_and_version()
    {
        var exe = IndexerExe();
        var help = Proc.Run(exe, ["help"], 30);
        Assert.Equal(0, help.ExitCode);
        foreach (var command in new[] { "init", "uninit", "status", "stop", "restart", "daemon", "serve", "report", "hook", "--version" })
            Assert.Contains("  " + command, help.Stdout);
        Assert.Equal(help.Stdout, Proc.Run(exe, ["--help"], 30).Stdout);
        Assert.Equal($"csharp-indexer {ToolVersion.Text}\n", Proc.Run(exe, ["--version"], 30).Stdout);
        Assert.Equal(2, Proc.Run(exe, ["bogus"], 30).ExitCode);
        Assert.Equal(2, Proc.Run(exe, ["status", "--bogus"], 30).ExitCode);
        Assert.Equal(2, Proc.Run(exe, ["stop", "--timeout", "x"], 30).ExitCode);
    }

    [Theory]
    [InlineData(5, "5s")]
    [InlineData(303, "5m 3s")]
    [InlineData(7500, "2h 5m")]
    [InlineData(273600, "3d 4h")]
    // Durations show their two largest units.
    public void test_duration(long seconds, string text) => Assert.Equal(text, Control.Duration(seconds));

    [Fact]
    // Only the indexer's own daemon process is ever taken for the one to kill: never zazzle's daemon or a client.
    public void test_is_indexer_daemon()
    {
        Assert.True(Control.IsIndexerDaemon("/opt/homebrew/opt/zazzle/libexec/zazzle/csharp-indexer daemon\n"));
        Assert.False(Control.IsIndexerDaemon("/opt/homebrew/opt/zazzle/libexec/zazzle/daemon serve"));
        Assert.False(Control.IsIndexerDaemon("/usr/sbin/daemon"));
        Assert.False(Control.IsIndexerDaemon("/x/csharp-indexer serve"));
    }
}
