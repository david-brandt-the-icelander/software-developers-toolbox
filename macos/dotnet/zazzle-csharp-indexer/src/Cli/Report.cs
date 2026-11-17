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
        var report = Build(records);
        var delivery = DeliveryPath();
        if (delivery is null)
            return report;
        var delivered = Read([delivery + ".1", delivery]);
        return delivered.Count == 0 ? report : report + "\n\n" + BuildDeliveries(delivered);
    }

    // Summarises the final responses zazzled returned after budgets and session deduplication.
    public static string BuildDeliveries(List<JsonNode> records)
    {
        var before = records.Sum(r => Int(r, "pre_dedupe_est_tokens"));
        var delivered = records.Sum(r => Int(r, "delivered_est_tokens"));
        var deduped = records.Where(r => r["deduplicated"]?.GetValue<bool>() == true).ToList();
        var saved = deduped.Sum(r => Math.Max(0, Int(r, "pre_dedupe_est_tokens") - Int(r, "delivered_est_tokens")));
        var sb = new StringBuilder();
        sb.AppendLine($"agent delivery: {records.Count} responses in {records.Select(r => Str(r, "session")).Distinct().Count()} sessions; ~{before} tokens before dedupe, ~{delivered} delivered");
        sb.AppendLine($"deduplicated: {deduped.Count}, ~{saved} tokens avoided");
        sb.AppendLine("by command (responses, delivered tokens, deduplicated):");
        foreach (var g in records.GroupBy(r => Str(r, "command")).OrderByDescending(g => g.Count()))
            sb.AppendLine($"  {g.Key}: {g.Count()}, {g.Sum(r => Int(r, "delivered_est_tokens"))}, {g.Count(r => r["deduplicated"]?.GetValue<bool>() == true)}");
        return sb.ToString().TrimEnd();
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
        var views = calls.Where(r => Int(r, "source_est_tokens") > 0 && Str(r, "reason") != "no_gain").ToList();
        if (views.Count > 0)
        {
            var source = views.Sum(r => Int(r, "source_est_tokens"));
            var served = views.Sum(r => Int(r, "est_tokens"));
            sb.AppendLine($"\nview compaction: ~{source} source tokens to ~{served} response tokens ({Math.Max(0, source - served)} saved)");
            Top(sb, "view transformation sets", views.Select(r => Str(r, "transformations")).Where(s => s != ""));
        }
        Top(sb, "reasons for non-index answers", calls.Where(r => Str(r, "path") != "index" && Str(r, "reason") != "").Select(r => $"{Str(r, "path")} / {Str(r, "reason")}"));
        Top(sb, "raw commands the hook redirected", all.Where(r => Str(r, "tool") == "hook").Select(r => Str(r, "reason")));
        sb.AppendLine("\nwaste signals:");
        sb.AppendLine($"  retries (same tool within 10 s, different arguments): {Retries(calls)}");
        sb.AppendLine($"  repeated identical calls in a session: {calls.GroupBy(r => Str(r, "session") + Str(r, "tool") + r["args"]?.ToJsonString()).Sum(g => g.Count() - 1)}");
        var viewCalls = calls.Where(r => Str(r, "tool") == "view").ToList();
        if (viewCalls.Count > 0)
        {
            var sized = viewCalls.Where(r => r["view_bytes"] is not null && Int(r, "view_bytes") >= 0 && Int(r, "source_bytes") > 0).ToList();
            var saving = sized.Count == 0 ? 0 : (int)sized.Average(r => 100.0 - 100.0 * Int(r, "view_bytes") / Int(r, "source_bytes"));
            sb.AppendLine($"  viewCalls: {viewCalls.Count}, mean saving {saving}% over {sized.Count} sized, refused for no gain {viewCalls.Count(r => Str(r, "reason") == "no_gain")}, same file viewed again in a session {viewCalls.GroupBy(r => Str(r, "session") + r["args"]?["path"]?.ToJsonString()).Sum(g => g.Count() - 1)}");
        }
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

    // Reads every valid JSON line from the named files in order.
    private static List<JsonNode> Read(IEnumerable<string> files)
    {
        var records = new List<JsonNode>();
        foreach (var file in files.Where(File.Exists))
            records.AddRange(File.ReadLines(file).Select(line => { try { return JsonNode.Parse(line); } catch (System.Text.Json.JsonException) { return null; } }).Where(node => node is not null)!);
        return records;
    }

    // Finds zazzled's final-response log, with an empty override disabling it.
    private static string? DeliveryPath()
    {
        if (Environment.GetEnvironmentVariable("ZAZZLE_USAGE_LOG") is { } over)
            return over.Length == 0 ? null : over;
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        return home.Length == 0 ? null : Path.Combine(home, ".zazzle", "usage.jsonl");
    }

    // Reads a string field, empty when missing or null.
    private static string Str(JsonNode r, string key) => r[key]?.GetValue<string>() ?? "";

    // Reads an integer field, 0 when missing.
    private static int Int(JsonNode r, string key) => r[key] is { } v ? (int)v.GetValue<long>() : 0;
}
