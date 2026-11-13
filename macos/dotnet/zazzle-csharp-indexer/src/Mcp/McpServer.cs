// Minimal MCP server over newline-delimited JSON-RPC: zazzled's session with the daemon, which lists no tools to clients.
using System.Diagnostics;
using System.Text.Json.Nodes;
using ZazzleIndexer.Tools;

namespace ZazzleIndexer.Mcp;

// Dispatches tool calls from zazzled by name and logs each call; tools/list is empty, since agents go through zazzle.
public sealed class McpServer
{
    private const string Instructions =
        "This server offers no tools to agents: use zazzle (MCP server `zazzle`) for everything. It reads, searches and edits files (zazzle_read, zazzle_find, zazzle_edit), and answers C# structure from this indexer: zazzle_overview, zazzle_find_symbol, zazzle_refs, zazzle_changed_since, zazzle_git_log, zazzle_git_blame, zazzle_report_gap.";

    private readonly Dictionary<string, ITool> _tools;
    private readonly UsageLog _log;
    private readonly string _session = Guid.NewGuid().ToString("N")[..8];

    // Builds the server with its roots and log.
    public McpServer(IReadOnlyList<Root> roots, UsageLog log)
    {
        _log = log;
        var set = new RootSet(roots);
        ITool[] tools = [new ViewTool(set), new OverviewTool(set), new FindSymbolTool(set), new ChangedSinceTool(set), new GitLogTool(set), new GitBlameTool(set), new ReportGapTool()];
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

    // Builds the tools/list result: no tools, because zazzle is the front door and calls these by name.
    private static JsonObject ListTools() => new() { ["tools"] = new JsonArray() };

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
