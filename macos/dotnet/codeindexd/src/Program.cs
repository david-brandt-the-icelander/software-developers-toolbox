// Entry point: parses the command line and starts the stdio MCP server.
using CodeIndexd;
using CodeIndexd.Mcp;

if (args.Length == 0 || args[0] is "--help" or "-h")
{
    Console.Error.WriteLine("usage: codeindexd serve [--root name=path]... (default: current directory)");
    return args.Length == 0 ? 2 : 0;
}
if (args[0] == "--version")
{
    Console.WriteLine(ToolVersion.Text);
    return 0;
}
if (args[0] != "serve")
{
    Console.Error.WriteLine($"unknown command: {args[0]}");
    return 2;
}

var roots = new List<Root>();
for (var i = 1; i < args.Length; i++)
{
    if (args[i] != "--root" || i + 1 >= args.Length)
    {
        Console.Error.WriteLine($"bad argument: {args[i]}");
        return 2;
    }
    roots.Add(Root.Parse(args[++i]));
}
if (roots.Count == 0)
    roots.Add(Root.Parse(Directory.GetCurrentDirectory()));

var log = UsageLog.Open();
var server = new McpServer(roots, log);
await server.RunAsync(Console.In, Console.Out);
return 0;
