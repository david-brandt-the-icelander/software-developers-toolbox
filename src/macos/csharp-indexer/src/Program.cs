// Entry point: parses the command line and runs one command (setup, daemon control, the hook, the daemon or a stdio MCP session).
using CSharpIndexer;
using System.Text.Json.Nodes;
using CSharpIndexer.Cli;
using CSharpIndexer.Daemon;
using CSharpIndexer.Mcp;

// The help text: every command, as people run it through zazzle.
const string Usage = """
    usage: zazzle csharp-indexer COMMAND (or csharp-indexer COMMAND)

    project setup:
      init [--dir DIR]             wire zazzle and the redirect hook into DIR's .mcp.json and .claude/settings.json (default: current folder)
      uninit [--dry-run] [DIR]     remove what init wrote there, leaving everything else as it was

    the shared daemon:
      status [--json]              whether it runs: pid, version, uptime, socket, state folder, memory, clients and indexed roots (exit 3: not running)
      stop [--timeout SECONDS]     let it finish its clients (default 10 s), then kill it
      restart [--timeout SECONDS]  stop it, then start a fresh one from this executable
      daemon                       run it in the foreground (clients start it on demand)
      serve [--no-daemon] [--root NAME=PATH]...
                                   an MCP session over stdio through the daemon, for debugging

    other:
      report                       what to improve next, from the usage logs
      hook                         the Claude Code PreToolUse hook init installs (hook JSON on stdin)
      help, --help, -h             this text
      --version                    print the version
    """;

if (args.Length == 0)
{
    Console.Error.WriteLine(Usage);
    return 2;
}
if (args[0] is "help" or "--help" or "-h")
{
    Console.WriteLine(Usage);
    return 0;
}
if (args[0] == "--version")
{
    Console.WriteLine("csharp-indexer " + ToolVersion.Text);
    return 0;
}
if (args[0] == "status")
{
    if (args.Skip(1).FirstOrDefault(a => a != "--json") is { } bad)
        return BadArgument(bad);
    return Control.Status(UsageLog.DefaultDir(), args.Contains("--json"), Console.Out);
}
if (args[0] is "stop" or "restart")
{
    var timeout = 10.0;
    if (args.Length > 1 && (args.Length != 3 || args[1] != "--timeout" || !double.TryParse(args[2], System.Globalization.CultureInfo.InvariantCulture, out timeout) || timeout < 0))
        return BadArgument(string.Join(' ', args[1..]));
    return args[0] == "stop"
        ? Control.Stop(UsageLog.DefaultDir(), TimeSpan.FromSeconds(timeout), Console.Out)
        : Control.Restart(UsageLog.DefaultDir(), Environment.ProcessPath!, TimeSpan.FromSeconds(timeout), Console.Out);
}
if (args[0] == "uninit")
{
    var dryRun = args.Contains("--dry-run");
    var dirs = args.Skip(1).Where(a => a != "--dry-run").ToList();
    if (dirs.Count > 1 || dirs.Any(d => d.StartsWith('-')))
        return BadArgument(string.Join(' ', dirs));
    var target = Path.GetFullPath(dirs.FirstOrDefault() ?? Directory.GetCurrentDirectory());
    if (!Directory.Exists(target))
    {
        Console.Error.WriteLine($"not a folder: {target}");
        return 2;
    }
    foreach (var line in Uninit.Apply(target, dryRun))
        Console.WriteLine(line);
    return 0;
}
if (args[0] == "report")
{
    Console.WriteLine(Report.FromDir(UsageLog.DefaultDir()));
    return 0;
}
if (args[0] == "init")
{
    var dir = args.Length > 2 && args[1] == "--dir" ? args[2] : Directory.GetCurrentDirectory();
    foreach (var line in Init.Apply(Path.GetFullPath(dir), Environment.ProcessPath!, Init.FindDaemon(Environment.ProcessPath!)))
        Console.WriteLine(line);
    Console.WriteLine("\nAdd this to AGENTS.md or CLAUDE.md:\n\n" + Init.AgentsSnippet);
    return 0;
}
if (args[0] == "hook")
{
    var input = JsonNode.Parse(Console.In.ReadToEnd());
    var tool = input?["tool_name"]?.GetValue<string>() ?? "";
    var decision = HookPolicy.Decide(tool, input?["tool_input"]?["command"]?.GetValue<string>(), input?["tool_input"]?["file_path"]?.GetValue<string>());
    if (decision.Program != "")
        UsageLog.Open().Record(input?["session_id"]?.GetValue<string>() ?? "", "hook", new JsonObject { ["tool"] = tool, ["program"] = decision.Program }, new CallInfo { Path = "hook", Reason = (decision.Block ? "blocked:" : "allowed_raw:") + decision.Program.Replace("allow-raw:", "") }, 0, "", null);
    if (!decision.Block)
        return 0;
    Console.Error.WriteLine(decision.Message);
    return 2;
}

if (args[0] is not ("serve" or "daemon"))
{
    Console.Error.WriteLine($"unknown command: {args[0]} (run csharp-indexer help)");
    return 2;
}

var specs = new List<string>();
var noDaemon = Environment.GetEnvironmentVariable("ZAZZLE_INDEXER_NO_DAEMON") == "1";
for (var i = 1; i < args.Length; i++)
{
    if (args[i] == "--no-daemon")
        noDaemon = true;
    else if (args[i] == "--root" && i + 1 < args.Length)
        specs.Add(args[++i]);
    else
        return BadArgument(args[i]);
}
if (specs.Count == 0)
    specs.Add(Directory.GetCurrentDirectory());
specs = specs.Select(Absolute).ToList();

if (args[0] == "daemon")
{
    var stateDir = UsageLog.DefaultDir();
    Directory.CreateDirectory(stateDir);
    using var engine = Engine.Open(stateDir);
    var idle = TimeSpan.FromMinutes(double.Parse(Environment.GetEnvironmentVariable("ZAZZLE_INDEXER_IDLE_MINUTES") ?? "30"));
    using var daemon = new DaemonServer(engine, Proxy.SocketPath(stateDir), idle);
    daemon.Start();
    await daemon.Done;
    return 0;
}

if (!noDaemon)
{
    var socket = Proxy.Connect(UsageLog.DefaultDir(), specs, out var error);
    if (socket is not null)
    {
        await Proxy.Pump(socket);
        return 0;
    }
    Console.Error.WriteLine($"csharp-indexer: no daemon ({error}); serving in-process");
}

using var local = Engine.Open();
var roots = specs.Select(s => Root.Parse(s, local)).ToList();
await new McpServer(roots, local.Log).RunAsync(Console.In, Console.Out);
return 0;

// Says an argument is not understood and returns the usage exit code.
static int BadArgument(string arg)
{
    Console.Error.WriteLine($"bad argument: {arg} (run csharp-indexer help)");
    return 2;
}

// Makes a root spec absolute while keeping its optional name= prefix.
static string Absolute(string spec)
{
    var eq = spec.IndexOf('=');
    return eq < 0 ? Path.GetFullPath(spec) : spec[..eq] + "=" + Path.GetFullPath(spec[(eq + 1)..]);
}
