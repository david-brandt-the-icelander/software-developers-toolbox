// `status`, `stop` and `restart`: ask the shared daemon about itself, stop it after its clients finish, and start a fresh one.
using System.Diagnostics;
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace CSharpIndexer.Daemon;

// What a status request found: whether a daemon listens, its full status (a daemon with commands), its version, or why it gave neither.
public sealed record Probe(bool Running, JsonObject? Status, string? Version, string? Error);

// Client side of the daemon's commands; every method takes the state folder that holds the socket.
public static partial class Control
{
    public const int NotRunning = 3;
    // A version newer than any release: an older daemon, which has no stop command, drains for it as for a newer client.
    public const string DrainVersion = "99999.0.0-dev";
    private const int AnswerMs = 5000;
    private static readonly TimeSpan StartWait = TimeSpan.FromSeconds(10);

    // Asks the daemon for its status; the hello names no version, so an older daemon refuses it as too old (naming its own version) instead of draining.
    public static Probe Query(string stateDir)
    {
        JsonNode? reply;
        try
        {
            reply = Ask(Proxy.SocketPath(stateDir), new JsonObject { ["command"] = "status" });
        }
        catch (Exception e) when (e is IOException or System.Text.Json.JsonException)
        {
            return new Probe(true, null, null, $"the daemon does not answer ({e.Message})");
        }
        if (reply is null)
            return new Probe(false, null, null, null);
        if (reply["status"] is JsonObject status)
            return new Probe(true, status, Text(status["version"]), null);
        var version = Text(reply["version"]) ?? OlderVersion(Text(reply["error"]) ?? "");
        return version is null ? new Probe(true, null, null, Text(reply["error"]) ?? "unexpected reply") : new Probe(true, null, version, null);
    }

    // Prints the daemon's status as text or JSON and returns 0 when it runs and answers, 3 when none runs, 1 when it does not answer.
    public static int Status(string stateDir, bool json, TextWriter output)
    {
        var report = StatusJson(stateDir, Query(stateDir));
        output.WriteLine(json ? report.ToJsonString() : StatusText(report));
        if (report["running"]!.GetValue<bool>() == false)
            return NotRunning;
        return report["ok"]!.GetValue<bool>() ? 0 : 1;
    }

    // Builds the status report: this tool and version, whether the daemon runs and answers, and the daemon's own fields (its version as daemon_version).
    public static JsonObject StatusJson(string stateDir, Probe probe)
    {
        var report = new JsonObject
        {
            ["tool"] = "csharp-indexer",
            ["version"] = ToolVersion.Text,
            ["ok"] = probe.Running && probe.Error is null,
            ["running"] = probe.Running,
            ["socket"] = Proxy.SocketPath(stateDir),
            ["state_dir"] = stateDir,
        };
        if (probe.Error is not null)
            report["error"] = probe.Error;
        if (probe.Status is null && probe.Version is not null)
        {
            report["daemon_version"] = probe.Version;
            report["note"] = "details need a newer daemon: run zazzle csharp-indexer restart";
        }
        foreach (var (key, value) in probe.Status ?? [])
            report[key == "version" ? "daemon_version" : key] = value?.DeepClone();
        return report;
    }

    // Renders a status report as a few readable lines.
    public static string StatusText(JsonObject r)
    {
        var sb = new StringBuilder();
        if (r["running"]!.GetValue<bool>() == false)
            sb.AppendLine("csharp-indexer daemon: not running");
        else if (Text(r["error"]) is { } error)
            sb.AppendLine($"csharp-indexer daemon: not answering: {error}");
        else if (r["pid"] is null)
            sb.AppendLine($"csharp-indexer daemon: running, version {Text(r["daemon_version"])}; {Text(r["note"])}");
        else
            sb.AppendLine($"csharp-indexer daemon: running{(r["draining"]?.GetValue<bool>() == true ? ", stopping once its clients finish" : "")} (pid {r["pid"]}, version {Text(r["daemon_version"])}, up {Duration(Long(r["uptime_seconds"]))})");
        sb.AppendLine($"socket: {Text(r["socket"])}");
        sb.AppendLine($"state: {Text(r["state_dir"])}");
        if (r["memory"] is JsonObject mem)
            sb.AppendLine($"memory: {Megabytes(Long(mem["working_set_bytes"]))} working set, {Megabytes(Long(mem["managed_heap_bytes"]))} managed heap");
        if (r["clients"] is JsonArray clients)
        {
            sb.AppendLine($"clients: {clients.Count}");
            foreach (var c in clients)
                sb.AppendLine($"  session {(Text(c!["session"]) is { Length: > 0 } id ? id : "(unnamed)")}, roots {string.Join(", ", c["roots"]!.AsArray().Select(Text))}, connected {Duration(Long(c["connected_seconds"]))} ago");
        }
        if (r["roots"] is JsonArray roots)
        {
            sb.AppendLine($"roots: {roots.Count}");
            foreach (var x in roots)
                sb.AppendLine($"  {Text(x!["path"])}: {x["files"]} files, {(x["ready"]?.GetValue<bool>() == true ? "ready" : "indexing")}, {(x["age_seconds"] is null ? "never indexed" : $"updated {Duration(Long(x["age_seconds"]))} ago")}");
        }
        return sb.ToString().TrimEnd('\n');
    }

    // Stops the daemon and prints what happened; 0 when it stopped or none ran, 1 when it could not be stopped.
    public static int Stop(string stateDir, TimeSpan timeout, TextWriter output) => StopDaemon(stateDir, timeout, output) ? 0 : 1;

    // Stops the daemon if one runs, then starts a fresh one from exe and prints both; 0 when the new one answers.
    public static int Restart(string stateDir, string exe, TimeSpan timeout, TextWriter output)
    {
        if (!StopDaemon(stateDir, timeout, output))
            return 1;
        Proxy.SpawnDaemon(exe, stateDir);
        var end = DateTime.UtcNow + StartWait;
        while (DateTime.UtcNow < end)
        {
            var probe = Query(stateDir);
            if (probe.Status is { } s)
            {
                output.WriteLine($"started csharp-indexer daemon {Text(s["version"])} (pid {s["pid"]})");
                return 0;
            }
            Thread.Sleep(100);
        }
        output.WriteLine($"the new daemon did not answer within {StartWait.TotalSeconds:0} s (started from {exe})");
        return 1;
    }

    // Asks the daemon to drain (a stop command, or for an older daemon the hello of a newer client), waits for it to exit, and kills it after the timeout.
    private static bool StopDaemon(string stateDir, TimeSpan timeout, TextWriter output)
    {
        var sock = Proxy.SocketPath(stateDir);
        var before = Query(stateDir);
        if (!before.Running)
        {
            output.WriteLine("no csharp-indexer daemon was running");
            return true;
        }
        var pid = Long(before.Status?["pid"]) is var known and > 0 ? (int)known : (int?)null;
        var name = $"csharp-indexer daemon {before.Version ?? "of unknown version"}{(pid is null ? "" : $" (pid {pid})")}";
        var clients = 0;
        try
        {
            var asked = before.Status is not null || before.Version is not null;
            var reply = !asked ? null : before.Status is not null
                ? Ask(sock, new JsonObject { ["command"] = "stop" })
                : Ask(sock, new JsonObject { ["version"] = DrainVersion, ["roots"] = new JsonArray() });
            clients = (reply?["status"]?["clients"] as JsonArray)?.Count ?? 0;
        }
        catch (Exception e) when (e is IOException or System.Text.Json.JsonException)
        {
            output.WriteLine($"the daemon did not take the stop request ({e.Message})");
        }
        if (clients > 0)
            output.WriteLine($"waiting up to {timeout.TotalSeconds:0} s for {clients} client(s) of the {name} to finish");
        else if (before.Status is null && before.Version is not null)
            output.WriteLine($"asked the {name}, which predates the stop command, to finish its clients and exit (waiting up to {timeout.TotalSeconds:0} s)");
        if (before.Error is null && WaitGone(sock, pid, timeout))
        {
            output.WriteLine($"stopped {name}");
            return true;
        }
        pid ??= FindPid(sock);
        if (pid is null)
        {
            output.WriteLine($"the {name} did not stop within {timeout.TotalSeconds:0} s and its process could not be found; quit the apps that use it, or kill it");
            return false;
        }
        if (!name.Contains("(pid "))
            name += $" (pid {pid})";
        Kill(pid.Value);
        if (WaitGone(sock, pid, TimeSpan.FromSeconds(5)))
        {
            DeleteStale(sock);
            output.WriteLine($"killed {name}: {(before.Error is null ? $"it had not finished within {timeout.TotalSeconds:0} s" : "it did not answer")}");
            return true;
        }
        output.WriteLine($"could not stop the {name}");
        return false;
    }

    // Connects, sends one hello and returns the reply, or null when nothing listens on the socket.
    private static JsonNode? Ask(string sock, JsonObject hello)
    {
        using var socket = Proxy.TryConnect(sock);
        if (socket is null)
            return null;
        socket.ReceiveTimeout = AnswerMs;
        socket.SendTimeout = AnswerMs;
        return Proxy.Exchange(socket, hello);
    }

    // Waits until the daemon's process has exited (or, with no process id, until nothing answers on the socket); false on timeout.
    private static bool WaitGone(string sock, int? pid, TimeSpan timeout)
    {
        var end = DateTime.UtcNow + timeout;
        while (pid is null ? Listening(sock) : Alive(pid.Value))
        {
            if (DateTime.UtcNow >= end)
                return false;
            Thread.Sleep(50);
        }
        return true;
    }

    // Deletes a socket file a killed daemon left behind, unless something listens on it again.
    private static void DeleteStale(string sock)
    {
        if (Listening(sock))
            return;
        try
        {
            File.Delete(sock);
        }
        catch (IOException)
        {
        }
    }

    // True when something accepts connections on the socket.
    private static bool Listening(string sock)
    {
        using var s = Proxy.TryConnect(sock);
        return s is not null;
    }

    // True while a process runs; a zombie (exited, not yet reaped) counts as gone.
    private static bool Alive(int pid)
    {
        try
        {
            using var p = Process.GetProcessById(pid);
            if (p.HasExited)
                return false;
        }
        catch (Exception e) when (e is ArgumentException or InvalidOperationException)
        {
            return false;
        }
        var stat = $"/proc/{pid}/stat";
        if (!File.Exists(stat))
            return true;
        try
        {
            var text = File.ReadAllText(stat);
            return text.Length <= text.LastIndexOf(')') + 2 || text[text.LastIndexOf(')') + 2] != 'Z';
        }
        catch (IOException)
        {
            return false;
        }
    }

    // Kills one process (never pid 0 or below, which would signal a process group), ignoring one that has already gone.
    private static void Kill(int pid)
    {
        if (pid <= 0 || pid == Environment.ProcessId)
            return;
        try
        {
            using var p = Process.GetProcessById(pid);
            p.Kill();
        }
        catch (Exception e) when (e is ArgumentException or InvalidOperationException or System.ComponentModel.Win32Exception)
        {
        }
    }

    // Finds the daemon process that holds the socket with lsof, keeping only a csharp-indexer whose command line ends in "daemon", so a connected client (zazzle's own daemon among them) is never taken for it.
    private static int? FindPid(string sock)
    {
        var holders = Proc.Run("lsof", ["-t", "--", sock], 5).Stdout.Split('\n', StringSplitOptions.RemoveEmptyEntries);
        foreach (var line in holders)
            if (int.TryParse(line.Trim(), out var pid) && pid > 0 && pid != Environment.ProcessId &&
                IsIndexerDaemon(Proc.Run("ps", ["-o", "args=", "-p", pid.ToString()], 5).Stdout))
                return pid;
        return null;
    }

    // Tells whether a process's command line is the indexer's daemon: csharp-indexer run as `daemon`.
    public static bool IsIndexerDaemon(string args)
    {
        var a = args.Trim();
        return a.EndsWith(" daemon") && a.Contains("csharp-indexer");
    }

    // Takes an older daemon's version from its refusal text.
    private static string? OlderVersion(string error) => OlderPattern().Match(error) is { Success: true } m ? m.Groups[1].Value : null;

    // Matches "the indexer daemon is version X, newer than", which every daemon version says when it refuses a client.
    [GeneratedRegex(@"daemon is version (\S+), newer than")]
    private static partial Regex OlderPattern();

    // Reads a JSON string value, or null.
    private static string? Text(JsonNode? n) => n is JsonValue v && v.TryGetValue<string>(out var s) ? s : null;

    // Reads a JSON number as a long, or 0.
    private static long Long(JsonNode? n) => n is JsonValue v && v.TryGetValue<long>(out var l) ? l : 0;

    // Formats bytes as megabytes.
    private static string Megabytes(long bytes) => $"{bytes / 1048576.0:0.0} MB";

    // Formats seconds as the two largest units, such as 5m 3s or 2h 5m.
    public static string Duration(long seconds)
    {
        if (seconds < 60)
            return $"{seconds}s";
        if (seconds < 3600)
            return $"{seconds / 60}m {seconds % 60}s";
        if (seconds < 86400)
            return $"{seconds / 3600}h {seconds % 3600 / 60}m";
        return $"{seconds / 86400}d {seconds % 86400 / 3600}h";
    }
}
