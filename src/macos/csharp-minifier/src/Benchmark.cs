// The --benchmark command: clones the root beside itself, transforms the clone, and runs each prompt three times in fresh Claude sessions.
using System.Text.Json.Nodes;

namespace CSharpMinifier;

// Prepares the benchmark- clone and runs and records the prompts.
internal static class Benchmark
{
    // Runs per prompt: the first warms caches, the second and third are compared.
    public const int Runs = 3;

    // Seconds one Claude session may take.
    private const int RunTimeoutSeconds = 3600;

    // The file that marks a folder as a clone this command made, so a later run may replace it.
    public const string Marker = "zazzle-benchmark";

    // Clones, transforms and runs every prompt; returns 0 when every session succeeded, 1 otherwise.
    public static int Run(ParsedArgs args, TextWriter output, TextWriter errors)
    {
        var prompts = PromptFiles(args.Benchmark!);
        if (prompts.Count == 0)
        {
            errors.WriteLine($"no .txt or .md prompt files in {args.Benchmark}");
            return 1;
        }
        var source = args.Root;
        var clone = Path.Combine(Path.GetDirectoryName(source)!, "benchmark-" + Path.GetFileName(source));
        if (CloneError(source, clone) is { } cloneError)
        {
            errors.WriteLine(cloneError);
            return 1;
        }
        output.WriteLine($"cloned {source} to {clone}");
        var transform = Transform(args, clone, output);
        var setup = new BenchmarkSetup(RepoIdentity.Of(source), clone, args.CommandSwitches(), args.Model!, args.Effort!,
            Proc.Run(ClaudeProgram(), ["--version"], 60).Output, transform);
        var failed = 0;
        foreach (var prompt in prompts)
            failed += RunPrompt(setup, prompt, output, errors) ? 0 : 1;
        return failed > 0 ? 1 : 0;
    }

    // The prompt files: the file itself, or the folder's .txt and .md files (not its subfolders) in name order.
    public static List<string> PromptFiles(string path)
    {
        if (File.Exists(path))
            return [path];
        return Directory.EnumerateFiles(path)
            .Where(f => f.EndsWith(".txt", StringComparison.OrdinalIgnoreCase) || f.EndsWith(".md", StringComparison.OrdinalIgnoreCase))
            .Order(StringComparer.Ordinal).ToList();
    }

    // Replaces an earlier clone this command made, then copies the source with copy-on-write where the file system has it; null on success.
    private static string? CloneError(string source, string clone)
    {
        if (Path.Exists(clone))
        {
            if (!IsOurClone(clone))
                return $"{clone} exists and is not a clone --benchmark made; move it away first";
            if (Proc.Run("rm", ["-rf", clone], 600).Code != 0)
                return $"could not remove the earlier clone {clone}";
        }
        var copied = OperatingSystem.IsMacOS()
            ? Proc.Run("cp", ["-Rc", source, clone], 3600).Code == 0 || Proc.Run("cp", ["-R", source, clone], 3600).Code == 0
            : Proc.Run("cp", ["-R", "--reflink=auto", source, clone], 3600).Code == 0;
        if (!copied)
            return $"could not copy {source} to {clone}";
        File.WriteAllText(MarkerPath(clone), $"clone of {source} made by csharp-minifier --benchmark\n");
        return null;
    }

    // True when the folder carries the marker of a benchmark clone.
    private static bool IsOurClone(string clone) =>
        File.Exists(Path.Combine(clone, ".git", Marker)) || File.Exists(Path.Combine(clone, "." + Marker));

    // Where the marker goes: inside .git when the clone has one, so the work tree the agent sees is unchanged.
    private static string MarkerPath(string clone) =>
        Directory.Exists(Path.Combine(clone, ".git")) ? Path.Combine(clone, ".git", Marker) : Path.Combine(clone, "." + Marker);

    // Runs the transforming switches on the clone and returns their output, or null when none were given.
    private static string? Transform(ParsedArgs args, string clone, TextWriter output)
    {
        if (args.CommandSwitches().Count == 0)
            return null;
        var log = new StringWriter();
        var code = Runner.Run(args with { Root = clone, Paths = [clone], Benchmark = null, Model = null, Effort = null }, log, log);
        output.WriteLine($"transformed the clone with {string.Join(" ", args.CommandSwitches())}" + (code == 0 ? "" : " (some files were left unchanged; see transform.txt)"));
        return log + $"exit code {code}\n";
    }

    // Runs one prompt three times, records every run and the summary, and prints one line; false when a session failed.
    private static bool RunPrompt(BenchmarkSetup setup, string prompt, TextWriter output, TextWriter errors)
    {
        var text = File.ReadAllText(prompt);
        var dir = BenchmarkStore.Create(setup, prompt, text);
        var runs = new List<RunResult>();
        for (var i = 1; i <= Runs; i++)
        {
            var (code, stdout, stderr) = Proc.RunWithInput(ClaudeProgram(), ClaudeArgs(setup), RunTimeoutSeconds, setup.ClonePath, text);
            var run = RunResult.Parse(i, code, stdout, stderr);
            BenchmarkStore.WriteRun(dir, run);
            runs.Add(run);
            if (run.Failed)
                errors.WriteLine($"{Path.GetFileName(prompt)}: run {i} failed: {run.Problem}");
        }
        var summary = BenchmarkStore.WriteSummary(dir, runs);
        output.WriteLine($"{Path.GetFileName(prompt)}: {string.Join(", ", runs.Select(r => $"run {r.Number} {r.TotalTokens:N0} tokens in {r.TurnCount} turns"))}; {summary} -> {dir}");
        return runs.All(r => !r.Failed);
    }

    // The arguments of one clean, non-interactive session: no saved session, a stream of every turn and tool call, and every tool that would ask for permission denied.
    public static List<string> ClaudeArgs(BenchmarkSetup setup) =>
        ["-p", "--model", setup.Model, "--effort", setup.Effort, "--output-format", "stream-json", "--verbose", "--no-session-persistence", "--permission-mode", "dontAsk"];

    // The Claude CLI to run: ZAZZLE_BENCHMARK_CLAUDE when set (tests use a stand-in), otherwise claude on PATH.
    private static string ClaudeProgram() => Environment.GetEnvironmentVariable("ZAZZLE_BENCHMARK_CLAUDE") is { Length: > 0 } p ? p : "claude";
}

// What every prompt of one benchmark shares: the source's identity, the clone, the switches, the model and effort, the CLI version and the transform log.
internal sealed record BenchmarkSetup(RepoIdentity Repo, string ClonePath, List<string> Switches, string Model, string Effort, string ClaudeVersion, string? TransformLog);

// The repository a benchmark ran on: its name, commit, whether it had uncommitted changes, its origin and its path.
internal sealed record RepoIdentity(string Name, string Sha, bool Dirty, string? Origin, string Path)
{
    // Reads the identity of the git work tree holding the folder; outside git the sha is "no-git".
    public static RepoIdentity Of(string folder)
    {
        var (code, sha) = Proc.Run("git", ["-C", folder, "rev-parse", "HEAD"], 20);
        if (code != 0)
            return new RepoIdentity(System.IO.Path.GetFileName(folder), "no-git", false, null, folder);
        var (_, up) = Proc.Run("git", ["-C", folder, "rev-parse", "--show-cdup"], 20);
        var top = System.IO.Path.GetFullPath(System.IO.Path.Combine(folder, up)).TrimEnd(System.IO.Path.DirectorySeparatorChar);
        var dirty = Proc.Run("git", ["-C", folder, "status", "--porcelain"], 60).Output.Length > 0;
        var (originCode, origin) = Proc.Run("git", ["-C", folder, "config", "--get", "remote.origin.url"], 20);
        return new RepoIdentity(System.IO.Path.GetFileName(top), sha, dirty, originCode == 0 ? origin : null, folder);
    }
}

// One session's outcome: the raw stream, its result event, its turns, and why it failed if it did.
internal sealed record RunResult(int Number, int ExitCode, string Raw, string Stderr, JsonObject? Json, List<Turn> Turns, string? Problem)
{
    // True when the CLI failed or reported an error.
    public bool Failed => Problem != null;

    // Input, output, cache-creation and cache-read tokens together, from the result event.
    public long TotalTokens => Usage("input_tokens") + Usage("output_tokens") + Usage("cache_creation_input_tokens") + Usage("cache_read_input_tokens");

    // The number of model responses: the result's num_turns, else the turns seen in the stream.
    public int TurnCount => Json?["num_turns"]?.GetValue<int>() ?? Turns.Count;

    // One usage count from the result, 0 when missing.
    public long Usage(string key) => BenchmarkStream.Count(Json?["usage"], key);

    // The tool names per turn, which two runs share when they took the same route.
    public string Route => string.Join(" | ", Turns.Select(t => string.Join(",", t.Tools.Select(c => c.Name))));

    // Reads the CLI's stream-json output (or a single JSON result) and notes why the run failed, if it did.
    public static RunResult Parse(int number, int code, string stdout, string stderr)
    {
        var events = BenchmarkStream.Events(stdout);
        var json = BenchmarkStream.Result(events) ?? (events.Count == 1 ? events[0] : null);
        string? problem = null;
        if (json == null)
            problem = $"exit code {code}, no JSON result: {First(stderr.Length > 0 ? stderr : stdout)}";
        else if (code != 0 || json["is_error"]?.GetValue<bool>() == true)
            problem = $"exit code {code}: {First(json["result"]?.ToString() ?? stderr)}";
        return new RunResult(number, code, stdout, stderr, json, BenchmarkStream.Turns(events), problem);
    }

    // The first line of a message, at most 200 characters.
    private static string First(string text)
    {
        var line = text.Split('\n')[0];
        return line.Length > 200 ? line[..200] : line;
    }
}
