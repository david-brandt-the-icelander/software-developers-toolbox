// Tests for --benchmark: the clone, the transform on it, the three sessions and the stored results, with a stand-in for the Claude CLI.
using System.Text.Json.Nodes;
using Xunit;

namespace ZazzleMinifier.Tests;

// Runs the benchmark against a fake claude script that answers with fixed JSON.
[System.Runtime.Versioning.UnsupportedOSPlatform("windows")]
public sealed class BenchmarkTests : IDisposable
{
    private readonly string parent = Directory.CreateTempSubdirectory("zazzle-csharp-minifier-bench").FullName;
    private readonly string results;
    private readonly string fake;

    // Creates a git repository with one commented C# file, a results folder and the fake CLI.
    public BenchmarkTests()
    {
        var source = Path.Combine(parent, "proj");
        Directory.CreateDirectory(source);
        File.WriteAllText(Path.Combine(source, "A.cs"), "// File.\nclass A { }\n");
        Git("init", "-q");
        Git("add", "-A");
        Git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x");
        results = Path.Combine(parent, "results");
        fake = Path.Combine(parent, "fake-claude");
        WriteFake(0);
        Environment.SetEnvironmentVariable("ZAZZLE_BENCHMARK_DIR", results);
        Environment.SetEnvironmentVariable("ZAZZLE_BENCHMARK_CLAUDE", fake);
    }

    // Removes the folders and the environment overrides.
    public void Dispose()
    {
        Environment.SetEnvironmentVariable("ZAZZLE_BENCHMARK_DIR", null);
        Environment.SetEnvironmentVariable("ZAZZLE_BENCHMARK_CLAUDE", null);
        Directory.Delete(parent, true);
    }

    // Writes the fake CLI: --version prints a version; otherwise it logs its arguments, folder and prompt, then prints a result whose tokens grow by one per call.
    private void WriteFake(int exitCode)
    {
        var log = Path.Combine(parent, "calls.log");
        var count = Path.Combine(parent, "count");
        var json = exitCode == 0
            ? "{\"type\":\"result\",\"is_error\":false,\"result\":\"ok\",\"num_turns\":2,\"duration_ms\":5,\"total_cost_usd\":0.01,\"usage\":{\"input_tokens\":100,\"output_tokens\":$n,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":1000},\"modelUsage\":{\"fake-model\":{}}}"
            : "";
        File.WriteAllText(fake, $$"""
            #!/bin/sh
            if [ "$1" = "--version" ]; then echo "9.9.9 (fake)"; exit 0; fi
            n=$(cat '{{count}}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{{count}}'
            echo "args: $*" >> '{{log}}'; echo "pwd: $(pwd -P)" >> '{{log}}'; echo "prompt: $(cat)" >> '{{log}}'
            {{(exitCode == 0 ? $"printf '{json.Replace("$n", "%s")}\\n' \"$n\"" : "echo 'boom' >&2")}}
            exit {{exitCode}}
            """);
        File.SetUnixFileMode(fake, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
    }

    // Runs git in the source repository.
    private void Git(params string[] args) => Proc.Run("git", ["-C", Path.Combine(parent, "proj"), .. args], 20);

    // Writes prompt files and returns their folder.
    private string Prompts()
    {
        var dir = Directory.CreateDirectory(Path.Combine(parent, "prompts")).FullName;
        File.WriteAllText(Path.Combine(dir, "explain.txt"), "Explain class A.");
        File.WriteAllText(Path.Combine(dir, "notes.json"), "{}");
        return dir;
    }

    // Parses and runs a benchmark command line, returning the exit code, output and errors.
    private (int Code, string Output, string Errors) Run(params string[] extra)
    {
        var parsed = Cli.Parse(["--benchmark", Prompts(), "--model", "fake-model", "--effort", "high", "--root", Path.Combine(parent, "proj"), .. extra]);
        Assert.Null(parsed.Error);
        var output = new StringWriter();
        var errors = new StringWriter();
        return (Runner.Run(parsed, output, errors), output.ToString(), errors.ToString());
    }

    [Fact]
    // A benchmark clones beside the source, transforms only the clone, runs each prompt file three times in the clone and stores everything under repo/sha/prompt.
    public void test_benchmark_runs_and_stores()
    {
        var (code, output, _) = Run("--strip-comments");
        Assert.Equal(0, code);
        var clone = Path.Combine(parent, "benchmark-proj");
        Assert.Equal("class A { }\n", File.ReadAllText(Path.Combine(clone, "A.cs")));
        Assert.Equal("// File.\nclass A { }\n", File.ReadAllText(Path.Combine(parent, "proj", "A.cs")));
        Assert.True(File.Exists(Path.Combine(clone, ".git", Benchmark.Marker)));
        var sha = Proc.Run("git", ["-C", Path.Combine(parent, "proj"), "rev-parse", "HEAD"], 20).Output;
        var runDir = Assert.Single(Directory.GetDirectories(Path.Combine(results, "proj", sha, "explain")));
        Assert.EndsWith("_fake-model_high_strip-comments", runDir);
        Assert.Equal(["meta.json", "prompt.txt", "run-1.json", "run-2.json", "run-3.json", "summary.json", "transform.txt"],
            Directory.GetFiles(runDir).Select(Path.GetFileName).Order(StringComparer.Ordinal));
        var meta = JsonNode.Parse(File.ReadAllText(Path.Combine(runDir, "meta.json")))!;
        Assert.Equal(sha, meta["sha"]!.GetValue<string>());
        Assert.Equal("9.9.9 (fake)", meta["claude_version"]!.GetValue<string>());
        Assert.False(Directory.Exists(Path.Combine(results, "proj", sha, "notes")));
        var summary = JsonNode.Parse(File.ReadAllText(Path.Combine(runDir, "summary.json")))!;
        Assert.Equal(1103, summary["runs"]![2]!["total_tokens"]!.GetValue<long>());
        Assert.True(summary["runs_2_and_3_agree"]!.GetValue<bool>());
        Assert.Contains("explain.txt: run 1 1,101 tokens, run 2 1,102 tokens, run 3 1,103 tokens; runs 2 and 3 agree", output);
        var log = File.ReadAllText(Path.Combine(parent, "calls.log"));
        Assert.Equal(3, log.Split("args: -p --model fake-model --effort high --output-format json --no-session-persistence --permission-mode dontAsk").Length - 1);
        Assert.Contains($"pwd: {Proc.Run("realpath", [clone], 20).Output}", log);
        Assert.Contains("prompt: Explain class A.", log);
    }

    [Fact]
    // A second benchmark replaces the clone the first one made.
    public void test_benchmark_replaces_own_clone()
    {
        Assert.Equal(0, Run().Code);
        File.WriteAllText(Path.Combine(parent, "benchmark-proj", "stale.txt"), "x");
        Assert.Equal(0, Run().Code);
        Assert.False(File.Exists(Path.Combine(parent, "benchmark-proj", "stale.txt")));
    }

    [Fact]
    // A benchmark- folder the command did not make is left alone and the benchmark stops.
    public void test_benchmark_refuses_foreign_folder()
    {
        var foreign = Directory.CreateDirectory(Path.Combine(parent, "benchmark-proj")).FullName;
        File.WriteAllText(Path.Combine(foreign, "mine.txt"), "keep");
        var (code, _, errors) = Run();
        Assert.Equal(1, code);
        Assert.Contains("is not a clone --benchmark made", errors);
        Assert.Equal("keep", File.ReadAllText(Path.Combine(foreign, "mine.txt")));
    }

    [Fact]
    // A session that fails is recorded with its error output and makes the exit code 1.
    public void test_benchmark_failed_session()
    {
        WriteFake(1);
        var (code, _, errors) = Run();
        Assert.Equal(1, code);
        Assert.Contains("explain.txt: run 1 failed: exit code 1, no JSON result: boom", errors);
        var runDir = Directory.GetDirectories(Directory.GetDirectories(Directory.GetDirectories(Path.Combine(results, "proj"))[0])[0])[0];
        Assert.Contains("boom", File.ReadAllText(Path.Combine(runDir, "run-1.json")));
    }

    [Fact]
    // Runs 2 and 3 agree within ten percent of the larger total.
    public void test_benchmark_agreement()
    {
        static RunResult R(int n, long tokens) => RunResult.Parse(n, 0, $"{{\"usage\":{{\"input_tokens\":{tokens}}}}}", "");
        Assert.Equal((true, 0.1), BenchmarkStore.Agreement([R(1, 5), R(2, 100), R(3, 90)]));
        Assert.False(BenchmarkStore.Agreement([R(1, 5), R(2, 100), R(3, 80)]).Agree);
        Assert.Null(BenchmarkStore.Agreement([R(1, 5), R(2, 100), RunResult.Parse(3, 1, "", "x")]).Agree);
    }

    [Fact]
    // --benchmark needs --model and --effort, a valid effort, and no checking, listing, renaming or paths.
    public void test_benchmark_parse_errors()
    {
        var prompts = Prompts();
        Assert.StartsWith("--benchmark, --model and --effort go together", Cli.Parse(["--benchmark", prompts, "--model", "m"]).Error);
        Assert.StartsWith("--effort must be", Cli.Parse(["--benchmark", prompts, "--model", "m", "--effort", "huge"]).Error);
        Assert.StartsWith("--benchmark takes --root", Cli.Parse(["--benchmark", prompts, "--model", "m", "--effort", "low", "--check"]).Error);
        Assert.StartsWith("benchmark prompt file or folder not found", Cli.Parse(["--benchmark", "/nope", "--model", "m", "--effort", "low"]).Error);
    }
}
