// Tests for symbol extraction and the structure tools zazzle calls.
using System.Text.Json.Nodes;
using ZazzleIndexer.Analysis;
using ZazzleIndexer.Tools;
using Xunit;

namespace ZazzleIndexer.Tests;

// Exercises Roslyn and heuristic extraction plus find_symbol, overview and changed_since.
public class SymbolTests
{
    private const string Cs = "namespace Demo;\n\n// Widget is documented here\npublic class Widget\n{\n    private int count;\n    public string Label { get; set; } = \"Widget label\";\n\n    public void Spin(int times)\n    {\n        count += times;\n    }\n}\n\npublic class Other\n{\n    void Use(Widget w) { w.Spin(2); }\n}\n";
    private const string Rs = "pub struct Engine {}\n\nimpl Engine {\n    pub fn start(&self) {}\n}\n\nfn main() {\n    let e = Engine {};\n}\n";

    private static readonly Dictionary<string, string> Files = new() { ["Widget.cs"] = Cs, ["engine.rs"] = Rs, ["README.md"] = "# Title\n\ntext\n\n## Usage\n" };

    // Runs a tool with JSON arguments.
    private static string Call(ITool tool, string json, CallInfo? info = null) => tool.Run(JsonNode.Parse(json), info ?? new CallInfo());

    [Fact]
    // Roslyn extraction finds types and members with containers and line spans.
    public void test_csharp_symbols()
    {
        var syms = CSharpSymbols.Extract(Cs);
        var spin = syms.Single(s => s.Name == "Spin");
        Assert.Equal(("method", "Demo.Widget", 9, 12), (spin.Kind, spin.Container, spin.Line, spin.EndLine));
        Assert.Contains(syms, s => s is { Name: "Label", Kind: "property" });
        Assert.Contains(syms, s => s is { Name: "count", Kind: "field" });
    }

    [Fact]
    // The heuristic extractor finds Rust items and Markdown headings.
    public void test_heuristic_symbols()
    {
        var rs = HeuristicSymbols.Extract(Rs, "rust");
        Assert.Equal(["Engine", "Engine", "start", "main"], rs.Select(s => s.Name));
        Assert.Equal(["type", "impl", "fn", "fn"], rs.Select(s => s.Kind));
        var md = HeuristicSymbols.Extract("# Title\n\n## Usage\n", "markdown");
        Assert.Equal(["Title", "Usage"], md.Select(s => s.Name));
    }

    [Fact]
    // find_symbol finds exact and partial names and filters by kind.
    public void test_find_symbol()
    {
        using var repo = new TempRepo(Files);
        var tool = new FindSymbolTool(new RootSet([repo.Root()]));
        Assert.Contains("r:Widget.cs:9-12 method Demo.Widget.Spin |", Call(tool, "{\"name\":\"spin\",\"exact\":true}"));
        Assert.Contains("r:engine.rs:", Call(tool, "{\"name\":\"engin\"}"));
        Assert.DoesNotContain("Widget.cs", Call(tool, "{\"name\":\"engin\"}"));
        Assert.EndsWith("[0 symbols]", Call(tool, "{\"name\":\"Widget\",\"kind\":\"method\"}"));
        Assert.Contains("r:Widget.cs:9-12 method Demo.Widget.Spin |", Call(tool, "{\"name\":\"Demo.Widget.Spin\",\"exact\":true}"));
        Assert.Contains("r:Widget.cs:9-12 method Demo.Widget.Spin |", Call(tool, "{\"name\":\"widget::spin\",\"exact\":true}"));
        Assert.Contains("Demo.Widget.Spin", Call(tool, "{\"name\":\"Widget.sp\"}"));
        Assert.EndsWith("[0 symbols]", Call(tool, "{\"name\":\"Other.Widget.Spin\",\"exact\":true}"));
        Assert.EndsWith("[0 symbols]", Call(tool, "{\"name\":\"emo.Widget.Spin\",\"exact\":true}"));
    }

    [Fact]
    // overview reports languages and entry files.
    public void test_overview()
    {
        using var repo = new TempRepo(Files);
        var text = Call(new OverviewTool(new RootSet([repo.Root()])), "{}");
        Assert.Contains("csharp 1 files/18 lines", text);
        Assert.Contains("entry files: README.md", text);
    }

    [Fact]
    // changed_since returns a cursor, then the files changed after it.
    public void test_changed_since()
    {
        using var repo = new TempRepo(Files);
        var tool = new ChangedSinceTool(new RootSet([repo.Root()]));
        var cursor = Call(tool, "{}").Replace("cursor: ", "");
        File.WriteAllText(Path.Combine(repo.Dir, "README.md"), "# Changed\n");
        File.WriteAllText(Path.Combine(repo.Dir, "new.txt"), "x\n");
        var text = Call(tool, $"{{\"cursor\":\"{cursor}\"}}");
        Assert.Contains("M r:README.md", text);
        Assert.Contains("A r:new.txt", text);
    }

    [Fact]
    // find_symbol puts exact names before prefix and substring matches, types before members and production before tests (partly matching tests after all production), and returns the end line.
    public void test_find_symbol_ranking()
    {
        var files = new Dictionary<string, string>
        {
            ["src/A.cs"] = "namespace N;\npublic class Other { public void RunWidget() { } }\npublic class WidgetFactory { }\npublic class Widget\n{\n    public void Make() { }\n}\n",
            ["tests/WidgetTests.cs"] = "namespace N.Tests;\npublic class WidgetTests { }\npublic class Widget { }\n",
        };
        using var repo = new TempRepo(files);
        var tool = new FindSymbolTool(new RootSet([repo.Root()]));
        var lines = Call(tool, "{\"name\":\"Widget\"}").Split('\n');
        Assert.StartsWith("r:src/A.cs:4-7 class N.Widget", lines[0]);
        Assert.StartsWith("r:tests/WidgetTests.cs:3 class N.Tests.Widget", lines[1]);
        Assert.Contains("WidgetFactory", lines[2]);
        Assert.Contains("RunWidget", lines[3]);
        Assert.Contains("class N.Tests.WidgetTests", lines[4]);
        var limited = Call(tool, "{\"name\":\"Widget\",\"limit\":2}");
        Assert.EndsWith("[2 of 5; next offset 2]", limited);
    }

    [Fact]
    // find_symbol shortens long signatures to one line.
    public void test_find_symbol_clips_signature()
    {
        var args = string.Join(", ", Enumerable.Range(1, 30).Select(i => $"int argument{i}"));
        using var repo = new TempRepo(new Dictionary<string, string> { ["B.cs"] = $"class B\n{{\n    void Long({args}) {{ }}\n}}\n" });
        var text = Call(new FindSymbolTool(new RootSet([repo.Root()])), "{\"name\":\"Long\",\"exact\":true}");
        Assert.Contains("…", text);
        Assert.True(text.Split('\n')[0].Length < 160);
    }

    [Fact]
    // overview honours .ignore and splits projects into production and test.
    public void test_overview_ignore_and_projects()
    {
        var files = new Dictionary<string, string>
        {
            [".ignore"] = "CHANGELOG/\n/docs/archive/\n*.g.cs\n",
            ["CHANGELOG/a.md"] = "# a\n",
            ["CHANGELOG/b.md"] = "# b\n",
            ["docs/archive/old.md"] = "# old\n",
            ["docs/keep.md"] = "# keep\n",
            ["Api/Gen.g.cs"] = "class G { }\n",
            ["Api/Api.csproj"] = "<Project/>\n",
            ["UnitTests.Api/UnitTests.Api.csproj"] = "<Project/>\n",
            ["Worker/Worker.csproj"] = "<Project/>\n",
            ["Api/Program.cs"] = "class P { }\n",
        };
        using var repo = new TempRepo(files);
        var text = Call(new OverviewTool(new RootSet([repo.Root()])), "{}");
        Assert.DoesNotContain("CHANGELOG", text);
        Assert.DoesNotContain("Gen.g.cs", text);
        Assert.Contains("hidden by .ignore", text);
        Assert.Contains("projects: 2 production: Api, Worker", text);
        Assert.Contains("test projects: 1: UnitTests.* 1", text);
        Assert.Contains("Api/Program.cs", text);
    }
}
