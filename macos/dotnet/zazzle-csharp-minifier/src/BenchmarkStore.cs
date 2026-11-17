// Stores benchmark results under ~/.zazzle/agent-token-benchmarks/<repo>/<sha>/<prompt>/<time>_<model>_<effort>_<switches>/.
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;

namespace ZazzleMinifier;

// Writes meta.json, prompt.txt, transform.txt, run-N.jsonl, run-N.json and summary.json for one prompt.
internal static class BenchmarkStore
{
    // Runs 2 and 3 agree when their token totals differ by at most this share of the larger.
    public const double AgreementTolerance = 0.10;

    // The results folder: ZAZZLE_BENCHMARK_DIR when set, otherwise ~/.zazzle/agent-token-benchmarks.
    public static string Root() =>
        Environment.GetEnvironmentVariable("ZAZZLE_BENCHMARK_DIR") is { Length: > 0 } dir
            ? dir
            : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".zazzle", "agent-token-benchmarks");

    // Creates this run's folder and writes what identifies it: meta.json, prompt.txt and, after a transform, transform.txt.
    public static string Create(BenchmarkSetup setup, string promptPath, string promptText)
    {
        var label = string.Join("_", DateTime.UtcNow.ToString("yyyyMMdd'T'HHmmss'Z'"), Safe(setup.Model), setup.Effort,
            setup.Switches.Count == 0 ? "baseline" : string.Join("+", setup.Switches.Select(s => s.TrimStart('-'))));
        var dir = Path.Combine(Root(), Safe(setup.Repo.Name), Safe(setup.Repo.Sha), Safe(Path.GetFileNameWithoutExtension(promptPath)), label);
        Directory.CreateDirectory(dir);
        File.WriteAllText(Path.Combine(dir, "prompt.txt"), promptText);
        if (setup.TransformLog != null)
            File.WriteAllText(Path.Combine(dir, "transform.txt"), setup.TransformLog);
        File.WriteAllText(Path.Combine(dir, "meta.json"), Meta(setup, promptPath, promptText).ToJsonString(Indented) + "\n");
        return dir;
    }

    // Writes one run's stream as run-N.jsonl and its result event (or the failure) as run-N.json.
    public static void WriteRun(string dir, RunResult run)
    {
        File.WriteAllText(Path.Combine(dir, $"run-{run.Number}.jsonl"), run.Raw + (run.Raw.EndsWith('\n') ? "" : "\n"));
        var text = run.Json != null
            ? run.Json.ToJsonString(Indented)
            : new JsonObject { ["exit_code"] = run.ExitCode, ["stdout"] = run.Raw, ["stderr"] = run.Stderr }.ToJsonString(Indented);
        File.WriteAllText(Path.Combine(dir, $"run-{run.Number}.json"), text + "\n");
    }

    // Writes summary.json and returns the agreement sentence for the console.
    public static string WriteSummary(string dir, List<RunResult> runs)
    {
        var (agree, spread) = Agreement(runs);
        var (sameTurns, sameRoute) = SameRoute(runs);
        var sentence = agree == null
            ? "runs 2 and 3 could not be compared"
            : $"runs 2 and 3 {(agree.Value ? "agree" : "differ")} ({spread:P1} apart{(sameTurns == true ? "" : $"; {runs[1].TurnCount} vs {runs[2].TurnCount} turns")}{(sameTurns == true && sameRoute == false ? "; different tools" : "")})";
        var summary = new JsonObject
        {
            ["runs"] = new JsonArray([.. runs.Select(RunSummary)]),
            ["runs_2_and_3_spread"] = spread,
            ["runs_2_and_3_agree"] = agree,
            ["runs_2_and_3_same_turns"] = sameTurns,
            ["runs_2_and_3_same_tools"] = sameRoute,
            ["agreement_tolerance"] = AgreementTolerance,
        };
        File.WriteAllText(Path.Combine(dir, "summary.json"), summary.ToJsonString(Indented) + "\n");
        return sentence;
    }

    // Whether runs 2 and 3 took the same number of turns and called the same tools in each; nulls when either failed.
    public static (bool? SameTurns, bool? SameTools) SameRoute(List<RunResult> runs)
    {
        if (runs.Count < 3 || runs[1].Failed || runs[2].Failed)
            return (null, null);
        return (runs[1].TurnCount == runs[2].TurnCount, runs[1].Route == runs[2].Route);
    }

    // How far apart runs 2 and 3 are, as a share of the larger total, and whether that is within the tolerance; null when either failed.
    public static (bool? Agree, double? Spread) Agreement(List<RunResult> runs)
    {
        if (runs.Count < 3 || runs[1].Failed || runs[2].Failed)
            return (null, null);
        double a = runs[1].TotalTokens, b = runs[2].TotalTokens;
        var spread = Math.Max(a, b) == 0 ? 0 : Math.Abs(a - b) / Math.Max(a, b);
        return (spread <= AgreementTolerance, spread);
    }

    // What identifies the run: repository, commit, prompt, switches, model, effort and CLI version.
    private static JsonObject Meta(BenchmarkSetup setup, string promptPath, string promptText) => new()
    {
        ["repo"] = setup.Repo.Name,
        ["repo_path"] = setup.Repo.Path,
        ["origin"] = setup.Repo.Origin,
        ["sha"] = setup.Repo.Sha,
        ["dirty"] = setup.Repo.Dirty,
        ["clone"] = setup.ClonePath,
        ["prompt_file"] = Path.GetFullPath(promptPath),
        ["prompt_sha256"] = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(promptText))).ToLowerInvariant(),
        ["switches"] = new JsonArray([.. setup.Switches.Select(s => (JsonNode)s)]),
        ["model"] = setup.Model,
        ["effort"] = setup.Effort,
        ["claude_version"] = setup.ClaudeVersion,
        ["claude_args"] = new JsonArray([.. Benchmark.ClaudeArgs(setup).Select(a => (JsonNode)a)]),
        ["runs"] = Benchmark.Runs,
        ["started_utc"] = DateTime.UtcNow.ToString("O"),
    };

    // One run's line in the summary: token counts, cost, turns, time and models used.
    private static JsonNode RunSummary(RunResult run) => new JsonObject
    {
        ["run"] = run.Number,
        ["failed"] = run.Failed,
        ["problem"] = run.Problem,
        ["total_tokens"] = run.TotalTokens,
        ["input_tokens"] = run.Usage("input_tokens"),
        ["output_tokens"] = run.Usage("output_tokens"),
        ["cache_creation_input_tokens"] = run.Usage("cache_creation_input_tokens"),
        ["cache_read_input_tokens"] = run.Usage("cache_read_input_tokens"),
        ["total_cost_usd"] = run.Json?["total_cost_usd"]?.DeepClone(),
        ["num_turns"] = run.TurnCount,
        ["tokens_per_turn"] = run.TurnCount == 0 ? null : run.TotalTokens / run.TurnCount,
        ["tool_calls"] = run.Turns.Sum(t => t.Tools.Count),
        ["denied_tool_calls"] = run.Turns.Sum(t => t.Tools.Count(c => c.Denied)),
        ["turns"] = new JsonArray([.. run.Turns.Select(TurnSummary)]),
        ["duration_ms"] = run.Json?["duration_ms"]?.DeepClone(),
        ["models"] = run.Json?["modelUsage"] is JsonObject models ? new JsonArray([.. models.Select(m => (JsonNode)m.Key)]) : null,
    };

    // One turn in the summary: its tokens and the tools it called, with what each was pointed at.
    private static JsonNode TurnSummary(Turn turn) => new JsonObject
    {
        ["turn"] = turn.Number,
        ["total_tokens"] = turn.Total,
        ["input_tokens"] = turn.Input,
        ["output_tokens"] = turn.Output,
        ["cache_creation_input_tokens"] = turn.CacheCreation,
        ["cache_read_input_tokens"] = turn.CacheRead,
        ["tools"] = new JsonArray([.. turn.Tools.Select(c => (JsonNode)new JsonObject
        {
            ["name"] = c.Name, ["target"] = c.Target, ["denied"] = c.Denied, ["failed"] = c.Failed,
        })]),
    };

    // A name safe as one folder name.
    private static string Safe(string name) =>
        new string(name.Select(c => char.IsAsciiLetterOrDigit(c) || c is '.' or '-' or '_' ? c : '-').ToArray());

    // Indented JSON for files people read.
    private static readonly System.Text.Json.JsonSerializerOptions Indented = new() { WriteIndented = true };
}
