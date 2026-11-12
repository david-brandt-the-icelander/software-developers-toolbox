// The contract every MCP tool implements, plus shared argument helpers.
using System.Text.Json.Nodes;

namespace CodeIndexd.Tools;

// A failure the agent should see as a tool error, with a short code for the log.
public sealed class ToolException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

// One callable MCP tool.
public interface ITool
{
    string Name { get; }
    string Description { get; }
    JsonNode Schema { get; }

    // Runs the tool and returns compact text; records its path and counts in info.
    string Run(JsonNode? args, CallInfo info);
}

// Argument and output helpers shared by tools.
public static class Args
{
    // Reads a required string argument.
    public static string Str(JsonNode? a, string key) =>
        a?[key]?.GetValue<string>() ?? throw new ToolException("bad_args", $"missing argument: {key}");

    // Reads an optional string argument.
    public static string? OptStr(JsonNode? a, string key) => a?[key]?.GetValue<string>();

    // Reads an optional integer argument with a default.
    public static int Int(JsonNode? a, string key, int fallback) => a?[key]?.GetValue<int>() ?? fallback;

    // Reads an optional boolean argument with a default.
    public static bool Bool(JsonNode? a, string key, bool fallback) => a?[key]?.GetValue<bool>() ?? fallback;

    // Builds an object schema from property name to JSON-schema type, with required names.
    public static JsonNode Schema(string[] required, params (string name, string type, string doc)[] props)
    {
        var p = new JsonObject();
        foreach (var (name, type, doc) in props)
            p[name] = new JsonObject { ["type"] = type, ["description"] = doc };
        var req = new JsonArray();
        foreach (var r in required)
            req.Add(r);
        return new JsonObject { ["type"] = "object", ["properties"] = p, ["required"] = req };
    }
}
