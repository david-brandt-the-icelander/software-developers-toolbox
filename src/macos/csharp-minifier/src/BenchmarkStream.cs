// Reads the Claude CLI's stream-json output into its result event and its turns, each with its tokens and tool calls.
using System.Text.Json.Nodes;

namespace CSharpMinifier;

// One tool call in a turn: the tool, what it was pointed at, and whether permission was denied or it failed.
internal sealed record ToolCall(string Id, string Name, string? Target, bool Denied, bool Failed);

// One model response: its message id, its token counts and the tools it called.
internal sealed record Turn(int Number, string MessageId, long Input, long Output, long CacheCreation, long CacheRead, List<ToolCall> Tools)
{
    // Input, output, cache-creation and cache-read tokens of this response together.
    public long Total => Input + Output + CacheCreation + CacheRead;
}

// Splits a stream-json transcript into the final result event and the turns.
internal static class BenchmarkStream
{
    // The result event, or null when the stream has none.
    public static JsonObject? Result(IEnumerable<JsonObject> events) => events.LastOrDefault(e => Type(e) == "result");

    // The events of the stream, one JSON object per line, skipping lines that are not JSON.
    public static List<JsonObject> Events(string stdout)
    {
        var events = new List<JsonObject>();
        foreach (var line in stdout.Split('\n'))
        {
            try
            {
                if (JsonNode.Parse(line) is JsonObject e)
                    events.Add(e);
            }
            catch (System.Text.Json.JsonException)
            {
            }
        }
        return events;
    }

    // The turns in order: one per assistant message id, its usage as last reported, its tool calls marked denied or failed from the events that follow.
    public static List<Turn> Turns(List<JsonObject> events)
    {
        var order = new List<string>();
        var usage = new Dictionary<string, JsonNode?>();
        var tools = new Dictionary<string, List<(string Id, string Name, string? Target)>>();
        var denied = new HashSet<string>();
        var failed = new HashSet<string>();
        foreach (var e in events)
        {
            if (Type(e) == "assistant" && e["message"] is JsonObject message && message["id"]?.ToString() is { } id)
                AddAssistant(message, id, order, usage, tools);
            else if (Type(e) == "system" && e["subtype"]?.ToString() == "permission_denied" && e["tool_use_id"]?.ToString() is { } deniedId)
                denied.Add(deniedId);
            else if (Type(e) == "user")
                AddFailures(e, failed);
        }
        return order.Select((id, i) => new Turn(i + 1, id, Count(usage[id], "input_tokens"), Count(usage[id], "output_tokens"),
            Count(usage[id], "cache_creation_input_tokens"), Count(usage[id], "cache_read_input_tokens"),
            tools[id].Select(t => new ToolCall(t.Id, t.Name, t.Target, denied.Contains(t.Id), failed.Contains(t.Id) && !denied.Contains(t.Id))).ToList())).ToList();
    }

    // Records an assistant event: a new turn when its message id is new, its latest usage, and its tool_use blocks.
    private static void AddAssistant(JsonObject message, string id, List<string> order,
        Dictionary<string, JsonNode?> usage, Dictionary<string, List<(string, string, string?)>> tools)
    {
        if (!usage.ContainsKey(id))
        {
            order.Add(id);
            tools[id] = [];
        }
        usage[id] = message["usage"];
        foreach (var block in message["content"]?.AsArray() ?? [])
            if (block?["type"]?.ToString() == "tool_use")
                tools[id].Add((block["id"]?.ToString() ?? "", block["name"]?.ToString() ?? "?", Target(block["input"] as JsonObject)));
    }

    // Notes the tool calls whose results came back as errors.
    private static void AddFailures(JsonObject e, HashSet<string> failed)
    {
        foreach (var block in e["message"]?["content"] as JsonArray ?? [])
            if (block?["type"]?.ToString() == "tool_result" && block["is_error"]?.GetValue<bool>() == true && block["tool_use_id"]?.ToString() is { } id)
                failed.Add(id);
    }

    // What a tool call was pointed at: its file, path, pattern, command or URL, at most 160 characters.
    private static string? Target(JsonObject? input)
    {
        var value = new[] { "file_path", "path", "pattern", "command", "url", "query", "description" }
            .Select(k => input?[k]?.ToString()).FirstOrDefault(v => !string.IsNullOrEmpty(v));
        return value == null ? null : value.Length > 160 ? value[..160] : value;
    }

    // One count from a usage object, 0 when missing.
    public static long Count(JsonNode? usage, string key) => usage?[key]?.GetValue<long>() ?? 0;

    // The event's type.
    private static string? Type(JsonObject e) => e["type"]?.ToString();
}
