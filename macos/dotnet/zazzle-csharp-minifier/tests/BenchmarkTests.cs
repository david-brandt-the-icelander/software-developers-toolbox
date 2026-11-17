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

    // Writes the fake CLI: --version prints a version; otherwise it logs its arguments, folder and prompt, then streams a denied Bash turn, a Read turn and a result whose output tokens grow by one per call.
    private void WriteFake(int exitCode)
    {
        var log = Path.Combine(parent, "calls.log");
        var count = Path.Combine(parent, "count");
        var head = Path.Combine(parent, "stream-head.jsonl");
        File.WriteAllText(head, string.Join("\n",
            "{\"type\":\"system\",\"subtype\":\"init\"}",
            "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}],\"usage\":{\"input_tokens\":50,\"output_tokens\":1,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":500}}}",
            "{\"type\":\"system\",\"subtype\":\"permission_denied\",\"tool_use_id\":\"t1\"}",
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"is_error\":true}]}}",
            "{\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}],\"usage\":{\"input_tokens\":50,\"output_tokens\":1,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":500}}}",
            "{\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t2\",\"name\":\"Read\",\"input\":{\"file_path\":\"A.cs\"}}],\"usage\":{\"input_tokens\":50,\"output_tokens\":2,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":500}}}",
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t2\"}]}}") + "\n");
        var result = "{\"type\":\"result\",\"is_error\":false,\"result\":\"ok\",\"num_turns\":2,\"duration_ms\":5,\"total_cost_usd\":0.01,\"usage\":{\"input_tokens\":100,\"output_tokens\":%s,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":1000},\"modelUsage\":{\"fake-model\":{}}}";
        File.WriteAllText(fake, $$"""
            #!/bin/sh
            if [ "$1" = "--version" ]; then echo "9.9.9 (fake)"; exit 0; fi
            n=$(cat '{{count}}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{{count}}'
            echo "args: $*" >> '{{log}}'; echo "pwd: $(pwd -P)" >> '{{log}}'; echo "prompt: $(cat)" >> '{{log}}'
            {{(exitCode == 0 ? $"cat '{head}'; printf '{result}\\n' \"$n\"" : "echo 'boom' >&2")}}
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
        Assert.Equal(["meta.json", "prompt.txt", "run-1.json", "run-1.jsonl", "run-2.json", "run-2.jsonl", "run-3.json", "run-3.jsonl", "summary.json", "transform.txt"],
            Directory.GetFiles(runDir).Select(Path.GetFileName).Order(StringComparer.Ordinal));
        var meta = JsonNode.Parse(File.ReadAllText(Path.Combine(runDir, "meta.json")))!;
        Assert.Equal(sha, meta["sha"]!.GetValue<string>());
        Assert.Equal("9.9.9 (fake)", meta["claude_version"]!.GetValue<string>());
        Assert.False(Directory.Exists(Path.Combine(results, "proj", sha, "notes")));
        var summary = JsonNode.Parse(File.ReadAllText(Path.Combine(runDir, "summary.json")))!;
        Assert.Equal(1103, summary["runs"]![2]!["total_tokens"]!.GetValue<long>());
        Assert.True(summary["runs_2_and_3_agree"]!.GetValue<bool>());
        Assert.True(summary["runs_2_and_3_same_turns"]!.GetValue<bool>());
        Assert.True(summary["runs_2_and_3_same_tools"]!.GetValue<bool>());
        var run = summary["runs"]![0]!;
        Assert.Equal(550, run["tokens_per_turn"]!.GetValue<long>());
        Assert.Equal(1, run["denied_tool_calls"]!.GetValue<int>());
        Assert.Equal(551, run["denied_turn_tokens"]!.GetValue<long>());
        Assert.Equal(2, run["tool_calls"]!.GetValue<int>());
        var turns = run["turns"]!.AsArray();
        Assert.Equal(2, turns.Count);
        Assert.Equal("Bash", turns[0]!["tools"]![0]!["name"]!.GetValue<string>());
        Assert.Equal("ls", turns[0]!["tools"]![0]!["target"]!.GetValue<string>());
        Assert.True(turns[0]!["tools"]![0]!["denied"]!.GetValue<bool>());
        Assert.False(turns[0]!["tools"]![0]!["failed"]!.GetValue<bool>());
        Assert.Equal("A.cs", turns[1]!["tools"]![0]!["target"]!.GetValue<string>());
        Assert.Equal(552, turns[1]!["total_tokens"]!.GetValue<long>());
        Assert.Contains("explain.txt: run 1 1,101 tokens in 2 turns, run 2 1,102 tokens in 2 turns, run 3 1,103 tokens in 2 turns; runs 2 and 3 agree", output);
        var log = File.ReadAllText(Path.Combine(parent, "calls.log"));
        Assert.Equal(3, log.Split("args: -p --model fake-model --effort high --output-format stream-json --verbose --no-session-persistence --permission-mode dontAsk").Length - 1);
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
    // Runs 2 and 3 with different turn counts or tools are reported as such.
    public void test_benchmark_same_route()
    {
        static RunResult R(int n, string tools)
        {
            var lines = tools.Split(' ').Select((t, i) =>
                $"{{\"type\":\"assistant\",\"message\":{{\"id\":\"m{i}\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"t{i}\",\"name\":\"{t}\",\"input\":{{}}}}]}}}}");
            var result = $"{{\"type\":\"result\",\"num_turns\":{tools.Split(' ').Length},\"usage\":{{\"input_tokens\":1}}}}";
            return RunResult.Parse(n, 0, string.Join("\n", lines.Append(result)), "");
        }
        Assert.Equal((true, true), BenchmarkStore.SameRoute([R(1, "Bash Read"), R(2, "Read Read"), R(3, "Read Read")]));
        Assert.Equal((true, false), BenchmarkStore.SameRoute([R(1, "Read"), R(2, "Read Grep"), R(3, "Read Read")]));
        Assert.Equal((false, false), BenchmarkStore.SameRoute([R(1, "Read"), R(2, "Read"), R(3, "Bash Read")]));
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
