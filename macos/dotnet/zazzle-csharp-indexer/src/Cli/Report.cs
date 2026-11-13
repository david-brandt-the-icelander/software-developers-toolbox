// `zazzle-csharp-indexer report`: summarises the usage log into the facts that decide what to build next.
using System.Text;
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Cli;

// Static aggregator over usage.jsonl records.
public static class Report
{
    // Reads the log files in the state directory (rotated file first) and builds the report.
    public static string FromDir(string stateDir)
    {
        var records = new List<JsonNode>();
        foreach (var f in new[] { Path.Combine(stateDir, "usage.jsonl.1"), Path.Combine(stateDir, "usage.jsonl") })
            if (File.Exists(f))
                records.AddRange(File.ReadLines(f).Select(l => { try { return JsonNode.Parse(l); } catch (System.Text.Json.JsonException) { return null; } }).Where(n => n is not null)!);
        return Build(records);
    }

    // Builds the report text from parsed records.
    public static string Build(List<JsonNode> all)
    {
        if (all.Count == 0)
            return "no usage recorded yet";
        var calls = all.Where(r => Str(r, "tool") != "hook").ToList();
        var sb = new StringBuilder();
        sb.AppendLine($"{calls.Count} calls in {calls.Select(r => Str(r, "session")).Distinct().Count()} sessions, {Str(all[0], "ts")[..10]} to {Str(all[^1], "ts")[..10]}; ~{calls.Sum(r => Int(r, "est_tokens"))} tokens served");
        sb.AppendLine("\nby tool (calls, avg ms, p95 ms, est tokens, truncated, errors):");
        foreach (var g in calls.GroupBy(r => Str(r, "tool")).OrderByDescending(g => g.Count()))
        {
            var ms = g.Select(r => Int(r, "ms")).Order().ToList();
            sb.AppendLine($"  {g.Key}: {g.Count()}, {(int)ms.Average()}, {ms[Math.Min(ms.Count - 1, (int)(ms.Count * 0.95))]}, {g.Sum(r => Int(r, "est_tokens"))}, {g.Count(r => r["truncated"]?.GetValue<bool>() == true)}, {g.Count(r => Str(r, "error") != "")}");
        }
        sb.AppendLine("\nhow answers were produced:");
        foreach (var g in calls.GroupBy(r => Str(r, "path")).OrderByDescending(g => g.Count()))
            sb.AppendLine($"  {g.Key}: {g.Count()} ({100 * g.Count() / calls.Count}%)");
        Top(sb, "reasons for non-index answers", calls.Where(r => Str(r, "path") != "index" && Str(r, "reason") != "").Select(r => $"{Str(r, "path")} / {Str(r, "reason")}"));
        var searches = calls.Where(r => Str(r, "tool") == "search" && Int(r, "files") > 0 && Int(r, "candidates") >= 0).ToList();
        if (searches.Count > 0)
            sb.AppendLine($"\nsearch selectivity: index kept on average {searches.Average(r => 100.0 * Int(r, "candidates") / Int(r, "files")):0.0}% of files as candidates over {searches.Count} searches");
        Top(sb, "run passthrough commands (program + flags): candidates for new tools", calls.Where(r => Str(r, "tool") == "run").Select(r => string.Join(' ', r["args"]?["argv"]?.AsArray().Select(a => a!.GetValue<string>()).Where((a, i) => i == 0 || a.StartsWith('-')) ?? [])));
        Top(sb, "raw commands the hook redirected", all.Where(r => Str(r, "tool") == "hook").Select(r => Str(r, "reason")));
        sb.AppendLine("\nwaste signals:");
        sb.AppendLine($"  retries (same tool within 10 s, different arguments): {Retries(calls)}");
        sb.AppendLine($"  repeated identical calls in a session: {calls.GroupBy(r => Str(r, "session") + Str(r, "tool") + r["args"]?.ToJsonString()).Sum(g => g.Count() - 1)}");
        sb.AppendLine($"  reads answered 'unchanged': {calls.Count(r => Str(r, "reason") == "unchanged_since_last_read")}");
        Top(sb, "errors", calls.Where(r => Str(r, "error") != "").Select(r => $"{Str(r, "tool")} / {Str(r, "error")}"));
        var gaps = calls.Where(r => Str(r, "note") != "").TakeLast(20).ToList();
        if (gaps.Count > 0)
        {
            sb.AppendLine("\ngap reports (latest 20):");
            foreach (var g in gaps)
                sb.AppendLine($"  {Str(g, "ts")[..10]} {Str(g, "note")}");
        }
        return sb.ToString().TrimEnd();
    }

    // Counts calls that follow another call of the same tool and session within ten seconds with different arguments.
    private static int Retries(List<JsonNode> calls)
    {
        var n = 0;
        foreach (var g in calls.GroupBy(r => Str(r, "session") + "|" + Str(r, "tool")))
        {
            JsonNode? prev = null;
            foreach (var r in g)
            {
                if (prev is not null && (DateTime.Parse(Str(r, "ts")) - DateTime.Parse(Str(prev, "ts"))).TotalSeconds <= 10 && r["args"]?.ToJsonString() != prev["args"]?.ToJsonString())
                    n++;
                prev = r;
            }
        }
        return n;
    }

    // Appends a titled top-10 count list when there is anything to show.
    private static void Top(StringBuilder sb, string title, IEnumerable<string> items)
    {
        var top = items.GroupBy(x => x).OrderByDescending(g => g.Count()).Take(10).ToList();
        if (top.Count == 0)
            return;
        sb.AppendLine($"\n{title}:");
        foreach (var g in top)
            sb.AppendLine($"  {g.Key}: {g.Count()}");
    }

    // Reads a string field, empty when missing or null.
    private static string Str(JsonNode r, string key) => r[key]?.GetValue<string>() ?? "";

    // Reads an integer field, 0 when missing.
    private static int Int(JsonNode r, string key) => r[key] is { } v ? (int)v.GetValue<long>() : 0;
}
