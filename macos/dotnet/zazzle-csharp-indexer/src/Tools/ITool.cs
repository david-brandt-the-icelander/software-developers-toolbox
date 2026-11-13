// The contract every internal tool implements, plus shared argument helpers.
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Tools;

// A failure the caller should see as a tool error, with a short code for the log.
public sealed class ToolException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

// One tool zazzled calls by name; clients are not shown it.
public interface ITool
{
    string Name { get; }

    // Runs the tool and returns compact text; records its path and counts in info.
    string Run(JsonNode? args, CallInfo info);
}

// Argument helpers shared by tools.
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
}
