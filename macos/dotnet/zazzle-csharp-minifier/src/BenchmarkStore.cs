// Stores benchmark results under ~/.zazzle/agent-token-benchmarks/<repo>/<sha>/<prompt>/<time>_<model>_<effort>_<switches>/.
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;

namespace ZazzleMinifier;

// Writes meta.json, prompt.txt, transform.txt, run-N.json and summary.json for one prompt.
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

    // Writes one run's raw CLI result, or the failure when there was no JSON.
    public static void WriteRun(string dir, RunResult run)
    {
        var text = run.Json != null
            ? run.Json.ToJsonString(Indented)
            : new JsonObject { ["exit_code"] = run.ExitCode, ["stdout"] = run.Raw, ["stderr"] = run.Stderr }.ToJsonString(Indented);
        File.WriteAllText(Path.Combine(dir, $"run-{run.Number}.json"), text + "\n");
    }

    // Writes summary.json and returns the agreement sentence for the console.
    public static string WriteSummary(string dir, List<RunResult> runs)
    {
        var (agree, spread) = Agreement(runs);
        var sentence = agree == null ? "runs 2 and 3 could not be compared" : $"runs 2 and 3 {(agree.Value ? "agree" : "differ")} ({spread:P1} apart)";
        var summary = new JsonObject
        {
            ["runs"] = new JsonArray([.. runs.Select(RunSummary)]),
            ["runs_2_and_3_spread"] = spread,
            ["runs_2_and_3_agree"] = agree,
            ["agreement_tolerance"] = AgreementTolerance,
        };
        File.WriteAllText(Path.Combine(dir, "summary.json"), summary.ToJsonString(Indented) + "\n");
        return sentence;
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
        ["num_turns"] = run.Json?["num_turns"]?.DeepClone(),
        ["duration_ms"] = run.Json?["duration_ms"]?.DeepClone(),
        ["models"] = run.Json?["modelUsage"] is JsonObject models ? new JsonArray([.. models.Select(m => (JsonNode)m.Key)]) : null,
    };

    // A name safe as one folder name.
    private static string Safe(string name) =>
        new string(name.Select(c => char.IsAsciiLetterOrDigit(c) || c is '.' or '-' or '_' ? c : '-').ToArray());

    // Indented JSON for files people read.
    private static readonly System.Text.Json.JsonSerializerOptions Indented = new() { WriteIndented = true };
}
