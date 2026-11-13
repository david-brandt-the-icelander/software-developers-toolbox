// Entry point: parses the command line and starts the stdio MCP server.
using ZazzleIndexer;
using System.Text.Json.Nodes;
using ZazzleIndexer.Cli;
using ZazzleIndexer.Daemon;
using ZazzleIndexer.Mcp;

if (args.Length == 0 || args[0] is "--help" or "-h")
{
    Console.Error.WriteLine("usage: zazzle-csharp-indexer serve [--no-daemon] [--root name=path]... | daemon | report | init [--dir path] | hook");
    return args.Length == 0 ? 2 : 0;
}
if (args[0] == "--version")
{
    Console.WriteLine("zazzle-csharp-indexer " + ToolVersion.Text);
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
    foreach (var line in Init.Apply(Path.GetFullPath(dir), Environment.ProcessPath!))
        Console.WriteLine(line);
    Console.WriteLine("\nAdd this to AGENTS.md or CLAUDE.md:\n\n" + Init.AgentsSnippet);
    return 0;
}
if (args[0] == "hook")
{
    var input = JsonNode.Parse(Console.In.ReadToEnd());
    var tool = input?["tool_name"]?.GetValue<string>() ?? "";
    var decision = HookPolicy.Decide(tool, input?["tool_input"]?["command"]?.GetValue<string>());
    if (decision.Program != "")
        UsageLog.Open().Record(input?["session_id"]?.GetValue<string>() ?? "", "hook", new JsonObject { ["tool"] = tool, ["program"] = decision.Program }, new CallInfo { Path = "hook", Reason = (decision.Block ? "blocked:" : "allowed_raw:") + decision.Program.Replace("allow-raw:", "") }, 0, 0, null);
    if (!decision.Block)
        return 0;
    Console.Error.WriteLine(decision.Message);
    return 2;
}

if (args[0] is not ("serve" or "daemon"))
{
    Console.Error.WriteLine($"unknown command: {args[0]}");
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
    {
        Console.Error.WriteLine($"bad argument: {args[i]}");
        return 2;
    }
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
    Console.Error.WriteLine($"zazzle-csharp-indexer: no daemon ({error}); serving in-process");
}

using var local = Engine.Open();
var roots = specs.Select(s => Root.Parse(s, local)).ToList();
await new McpServer(roots, local.Log).RunAsync(Console.In, Console.Out);
return 0;

// Makes a root spec absolute while keeping its optional name= prefix.
static string Absolute(string spec)
{
    var eq = spec.IndexOf('=');
    return eq < 0 ? Path.GetFullPath(spec) : spec[..eq] + "=" + Path.GetFullPath(spec[(eq + 1)..]);
}
