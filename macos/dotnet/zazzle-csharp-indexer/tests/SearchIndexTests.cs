// Tests for literal extraction, the Bloom prefilter and index-backed search.
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;
using ZazzleIndexer.Analysis;
using ZazzleIndexer.Tools;
using Xunit;

namespace ZazzleIndexer.Tests;

// Exercises the prefilter for soundness and the search tool end to end.
public class SearchIndexTests
{
    private const string Corpus = "public class FooBar { void Handle_Request(int x) { if (x > 0) return; /* TODO: refactor */ } }\nfoo.bar(\"Hello, World\"); // Ünïcode ÉCOLE straße\nlet re = /a+b*c?d{2}/;\n";

    [Theory]
    [InlineData("hello", "hello")]
    [InlineData("foo\\.bar", "foo.bar")]
    [InlineData("ab*cde", "cde")]
    [InlineData("x(abc|def)yz1", "yz1")]
    [InlineData("foo|bar", "")]
    [InlineData("\\d+abc\\s", "abc")]
    [InlineData("[a-z]+needle", "needle")]
    // Required literals come out as the runs every match must contain.
    public void test_literals_extracted(string pattern, string expected) =>
        Assert.Equal(expected, string.Join('|', RegexLiterals.Required(pattern, false)));

    [Theory]
    [InlineData("class\\s+FooBar")]
    [InlineData("handle_request")]
    [InlineData("(?i)todo: REFACTOR")]
    [InlineData("hello, world")]
    [InlineData("a+b*c?d\\{2\\}")]
    [InlineData("x(abc|def)?")]
    [InlineData("É?COLE")]
    [InlineData("STRASSE|straße")]
    [InlineData("fo{0,2}bar")]
    [InlineData("foo.bar\\(")]
    // The prefilter never rejects a file the regex matches, for a spread of patterns and both case modes.
    public void test_prefilter_is_sound(string pattern)
    {
        var bloom = Bloom.Build(Encoding.UTF8.GetBytes(Corpus));
        foreach (var ignore in new[] { false, true })
        {
            var options = RegexOptions.CultureInvariant | (ignore ? RegexOptions.IgnoreCase : RegexOptions.None);
            if (!Regex.IsMatch(Corpus, pattern, options))
                continue;
            var grams = Bloom.Grams(RegexLiterals.Required(pattern, false).Select(Bloom.Encode));
            Assert.True(Bloom.MayContainAll(bloom, grams), $"{pattern} ignore={ignore}");
        }
    }

    [Fact]
    // The Bloom filter rejects most files that lack the literal.
    public void test_bloom_rejects_absent_literal()
    {
        var bloom = Bloom.Build(Encoding.UTF8.GetBytes(Corpus));
        Assert.False(Bloom.MayContainAll(bloom, Bloom.Grams([Bloom.Encode("zebra giraffe")])));
    }

    // Runs a tool with JSON arguments.
    private static string Call(ITool tool, string json, CallInfo info) => tool.Run(JsonNode.Parse(json), info);

    [Fact]
    // Search narrows candidates with the index and still finds an edit made a moment ago.
    public void test_search_uses_index_and_sees_fresh_edit()
    {
        var files = new Dictionary<string, string>();
        for (var i = 0; i < 20; i++)
            files[$"f{i}.txt"] = $"line {i} common words here\n";
        files["target.txt"] = "the zebra giraffe lives here\n";
        using var repo = new TempRepo(files);
        var tool = new SearchTool(new RootSet([repo.Root()]));
        var info = new CallInfo();
        Assert.Contains("target.txt\n1:the zebra giraffe", Call(tool, "{\"pattern\":\"zebra giraffe\",\"literal\":true}", info));
        Assert.Equal(1, info.Candidates);
        Assert.Equal(21, info.TotalFiles);
        File.WriteAllText(Path.Combine(repo.Dir, "f3.txt"), "now with quokka marsupial\n");
        Assert.Contains("f3.txt\n1:now with quokka", Call(tool, "{\"pattern\":\"quokka marsupial\"}", new CallInfo()));
    }

    [Fact]
    // The files and count modes list matching files and per-file counts.
    public void test_search_modes()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "x needle\nneedle y\n", ["b.txt"] = "needle\n", ["c.txt"] = "nothing\n" });
        var tool = new SearchTool(new RootSet([repo.Root()]));
        Assert.Equal("r:a.txt\nr:b.txt\n[2 files]", Call(tool, "{\"pattern\":\"needle\",\"mode\":\"files\"}", new CallInfo()));
        Assert.Equal("r:a.txt:2\nr:b.txt:1\n[2 files]", Call(tool, "{\"pattern\":\"needle\",\"mode\":\"count\"}", new CallInfo()));
    }

    [Fact]
    // Case-insensitive search finds text whose case differs from the pattern.
    public void test_search_ignore_case_uses_folded_index()
    {
        using var repo = new TempRepo(new() { ["a.txt"] = "Hello WORLD\n" });
        var tool = new SearchTool(new RootSet([repo.Root()]));
        Assert.Contains("1:Hello WORLD", Call(tool, "{\"pattern\":\"hello world\",\"ignore_case\":true}", new CallInfo()));
    }
}
