// Minimal MCP server over newline-delimited JSON-RPC on stdio.
using System.Diagnostics;
using System.Text.Json.Nodes;
using ZazzleIndexer.Tools;

namespace ZazzleIndexer.Mcp;

// Dispatches MCP requests to tools and logs each call.
public sealed class McpServer
{
    private const string Instructions =
        "Use these tools instead of grep, rg, find, cat or sed for questions about the code in the configured roots. " +
        "Paths are written root:relative/path. Regex dialect: .NET (Perl-style: lookarounds, backreferences, \\d \\b \\s, inline flags).";

    private readonly Dictionary<string, ITool> _tools;
    private readonly UsageLog _log;
    private readonly string _session = Guid.NewGuid().ToString("N")[..8];

    // Builds the server with its roots and log.
    public McpServer(IReadOnlyList<Root> roots, UsageLog log)
    {
        _log = log;
        var set = new RootSet(roots);
        ITool[] tools = [new FindFilesTool(set), new ReadTool(set), new SearchTool(set), new OutlineTool(set), new FindSymbolTool(set), new FindReferencesTool(set), new OverviewTool(set), new ChangedSinceTool(set), new GitLogTool(set), new GitBlameTool(set), new RunTool(set), new ReportGapTool()];
        _tools = tools.ToDictionary(t => t.Name);
    }

    // Reads requests line by line until input ends.
    public async Task RunAsync(TextReader input, TextWriter output)
    {
        while (await input.ReadLineAsync() is { } line)
        {
            if (string.IsNullOrWhiteSpace(line))
                continue;
            var reply = Handle(line);
            if (reply is null)
                continue;
            await output.WriteLineAsync(reply.ToJsonString());
            await output.FlushAsync();
        }
    }

    // Handles one message and returns the reply, or null for notifications.
    public JsonObject? Handle(string line)
    {
        JsonNode? msg;
        try
        {
            msg = JsonNode.Parse(line);
        }
        catch (System.Text.Json.JsonException)
        {
            return Error(null, -32700, "parse error");
        }
        var id = msg?["id"]?.DeepClone();
        var method = msg?["method"]?.GetValue<string>();
        if (method is null)
            return id is null ? null : Error(id, -32600, "invalid request");
        if (id is null)
            return null;
        return method switch
        {
            "initialize" => Result(id, Initialize()),
            "ping" => Result(id, new JsonObject()),
            "tools/list" => Result(id, ListTools()),
            "tools/call" => Result(id, CallTool(msg!["params"])),
            _ => Error(id, -32601, $"method not found: {method}"),
        };
    }

    // Builds the initialize result.
    private static JsonObject Initialize() => new()
    {
        ["protocolVersion"] = "2024-11-05",
        ["capabilities"] = new JsonObject { ["tools"] = new JsonObject() },
        ["serverInfo"] = new JsonObject { ["name"] = "zazzle-csharp-indexer", ["version"] = ToolVersion.Text },
        ["instructions"] = Instructions,
    };

    // Builds the tools/list result.
    private JsonObject ListTools()
    {
        var arr = new JsonArray();
        foreach (var t in _tools.Values)
            arr.Add(new JsonObject { ["name"] = t.Name, ["description"] = t.Description, ["inputSchema"] = t.Schema.DeepClone() });
        return new JsonObject { ["tools"] = arr };
    }

    // Runs a tool, times it, logs it, and wraps its text (or error) as an MCP tool result.
    private JsonObject CallTool(JsonNode? p)
    {
        var name = p?["name"]?.GetValue<string>() ?? "";
        var args = p?["arguments"];
        var info = new CallInfo();
        var sw = Stopwatch.StartNew();
        string text;
        string? error = null;
        if (!_tools.TryGetValue(name, out var tool))
        {
            text = $"unknown tool: {name}";
            error = "unknown_tool";
        }
        else
        {
            try
            {
                text = tool.Run(args, info);
            }
            catch (ToolException e)
            {
                text = e.Message;
                error = e.Code;
            }
        }
        var bytes = System.Text.Encoding.UTF8.GetByteCount(text);
        _log.Record(_session, name, args, info, sw.ElapsedMilliseconds, bytes, error);
        return new JsonObject
        {
            ["content"] = new JsonArray(new JsonObject { ["type"] = "text", ["text"] = text }),
            ["isError"] = error is not null,
        };
    }

    // Wraps a result for a request id.
    private static JsonObject Result(JsonNode id, JsonNode result) => new() { ["jsonrpc"] = "2.0", ["id"] = id, ["result"] = result };

    // Wraps a JSON-RPC error.
    private static JsonObject Error(JsonNode? id, int code, string message) => new()
    {
        ["jsonrpc"] = "2.0",
        ["id"] = id,
        ["error"] = new JsonObject { ["code"] = code, ["message"] = message },
    };
}
