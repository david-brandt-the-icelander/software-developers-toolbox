// Tests for the terse C# view and its MCP tool.
using System.Text.Json.Nodes;
using CSharpIndexer.Tools;
using CSharpIndexer.View;
using Xunit;

namespace CSharpIndexer.Tests;

// Checks each view rule, the cases it must leave alone, and the tool's output.
public class ViewTests
{
    private const string Service = """
        using System;
        using Microsoft.Extensions.Logging;

        namespace Acme.Billing;

        #region Service
        /// <summary>Totals invoices.</summary>
        public sealed class InvoiceService
        {
            private readonly ILogger<InvoiceService> _logger;
            private readonly IRepo _repo;

            /// <summary>Makes one.</summary>
            public InvoiceService(ILogger<InvoiceService> logger, IRepo repo)
            {
                _logger = logger;
                _repo = repo ?? throw new ArgumentNullException(nameof(repo));
            }

            public decimal Total(int id)
            {
                _logger.LogInformation("Totalling {Id}", id);
                Invoice invoice = (Invoice)_repo.Get(id);
                List<int> seen = new List<int>();
                IRepo other = new Repo();
                long big = 1;
                try
                {
                    return invoice.Sum();
                }
                catch (InvalidOperationException e)
                {
                    _logger.LogError(e, "Failed {Id}", id);
                    throw;
                }
                catch (TimeoutException)
                {
                    return 0;
                }
            }
        }
        #endregion
        """;

    // Builds the view of the sample and returns its formatted text.
    private static string Formatted() => TerseView.Format(TerseView.Build(Service).Lines);

    [Fact]
    // Usings collapse to one note, regions and logging disappear, and line numbers stay those of the source.
    public void test_view_hides_noise_with_source_numbers()
    {
        var v = Formatted();
        Assert.StartsWith("1-2\t⋯ 2 using(s)\n3\t\n4\tnamespace Acme.Billing;\n5\t\n7\t/// <summary>Totals invoices.</summary>", v);
        Assert.DoesNotContain("#region", v);
        Assert.DoesNotContain("LogInformation", v);
        Assert.DoesNotContain("\n22\t", v);
    }

    [Fact]
    // A constructor that only assigns fields collapses to one note naming its parameters, doc comment included.
    public void test_view_collapses_assigning_constructor()
    {
        var v = Formatted();
        Assert.Contains("13-18\t⋯ ctor(ILogger<InvoiceService> logger, IRepo repo): only assigns fields", v);
        Assert.DoesNotContain("Makes one", v);
    }

    [Fact]
    // A catch that logs and rethrows collapses, but one that returns a value is behaviour and stays.
    public void test_view_collapses_only_rethrowing_catches()
    {
        var v = Formatted();
        Assert.Contains("\t⋯ catch (InvalidOperationException): logs and rethrows", v);
        Assert.Contains("catch (TimeoutException)", v);
        Assert.Contains("return 0;", v);
    }

    [Fact]
    // Access modifiers go and `var` replaces only types spelled out on the right, marking changed lines.
    public void test_view_shortens_declarations()
    {
        var v = Formatted();
        Assert.Contains("8~\tsealed class InvoiceService", v);
        Assert.Contains("11~\t    readonly IRepo _repo;", v);
        Assert.Contains("20~\t    decimal Total(int id)", v);
        Assert.Contains("~\t        var inv = (Invoice)_repo.Get(id);", v);
        Assert.Contains("29~\t            return inv.Sum();", v);
        Assert.Contains("~\t        var seen = new List<int>();", v);
        Assert.Contains("\t        IRepo other = new Repo();", v);
        Assert.Contains("\t        long big = 1;", v);
    }

    [Fact]
    // The steps count what each rule changed.
    public void test_view_counts_steps()
    {
        var steps = TerseView.Build(Service).Steps.ToDictionary(s => s.Step, s => s.Count);
        Assert.Equal(1, steps["usings"]);
        Assert.Equal(1, steps["constructors"]);
        Assert.Equal(1, steps["log-and-rethrow catches"]);
        Assert.Equal(2, steps["regions"]);
        Assert.Equal(2, steps["var"]);
        Assert.True(steps["log statements"] >= 1);
    }

    [Theory]
    // Logging calls are recognised on loggers, Debug, Trace and spans, and not when they change state or on other receivers.
    [InlineData("_logger.LogDebug(\"x\");", true)]
    [InlineData("this._logger.LogWarning(\"x\");", true)]
    [InlineData("Log.Information(\"x {A}\", a);", true)]
    [InlineData("Debug.WriteLine(\"x\");", true)]
    [InlineData("activity?.SetTag(\"k\", v);", true)]
    [InlineData("using var span = Source.StartActivity(\"x\");", true)]
    [InlineData("_logger.LogInformation(\"x {N}\", n++);", false)]
    [InlineData("_logger.LogInformation(\"x {N}\", await Get());", false)]
    [InlineData("_catalog.Information(\"x\");", false)]
    [InlineData("Console.WriteLine(\"x\");", false)]
    [InlineData("var x = 1;", false)]
    public void test_view_is_logging(string statement, bool expected) =>
        Assert.Equal(expected, TerseView.IsLogging(Microsoft.CodeAnalysis.CSharp.SyntaxFactory.ParseStatement(statement)));

    [Fact]
    // Logging inside an if without a block, guarded logging and statements sharing a line are handled conservatively.
    public void test_view_logging_edge_cases()
    {
        var v = TerseView.Format(TerseView.Build("""
            class C
            {
                void M(bool b)
                {
                    if (b) _logger.LogDebug("x");
                    if (_logger.IsEnabled(LogLevel.Debug))
                    {
                        _logger.LogDebug("y");
                    }
                    Run(); _logger.LogDebug("z");
                }
            }
            """).Lines);
        Assert.Contains("if (b) _logger.LogDebug(\"x\");", v);
        Assert.DoesNotContain("IsEnabled", v);
        Assert.Contains("Run(); _logger.LogDebug(\"z\");", v);
    }

    [Fact]
    // Constructors with logic, static constructors and empty ones are told apart.
    public void test_view_constructor_cases()
    {
        var v = TerseView.Format(TerseView.Build("""
            class C : B
            {
                public C(int a) { Check(a); _a = a; }
                public C() : base(1) { }
                static C() { _s = s; }
            }
            """).Lines);
        Assert.Contains("C(int a) { Check(a); _a = a; }", v);
        Assert.Contains("4-4\t⋯ ctor() : base(1 args): empty", v);
        Assert.Contains("static C() { _s = s; }", v);
    }

    [Fact]
    // A file with nothing to shorten comes back unchanged.
    public void test_view_plain_file_unchanged()
    {
        var (lines, steps) = TerseView.Build("class C\n{\n    int X() => 1;\n}\n");
        Assert.Empty(steps);
        Assert.Equal("1\tclass C\n2\t{\n3\t    int X() => 1;\n4\t}\n5\t", TerseView.Format(lines));
    }

    [Fact]
    // The tool returns a header with the step summary, refuses other files and missing files.
    public void test_view_tool()
    {
        using var repo = new TempRepo(new Dictionary<string, string> { ["src/S.cs"] = Service, ["notes.txt"] = "x\n" });
        var tool = new ViewTool(new RootSet([repo.Root()]));
        var info = new CallInfo();
        var text = tool.Run(JsonNode.Parse("""{"path":"r:src/S.cs"}"""), info);
        Assert.Matches(@"^r:src/S.cs \d+ lines; terse: usings, constructors, log-and-rethrow catches.*, saves \d+%\n", text);
        Assert.Contains("\n1-2\t⋯ 2 using(s)\n", text);
        Assert.True(info.SourceBytes > text.Length);
        Assert.True(info.SourceLines > 0);
        Assert.Contains("usings=1", info.Transformations);
        Assert.Equal("not a C# file: r:notes.txt", Assert.Throws<ToolException>(() => tool.Run(JsonNode.Parse("""{"path":"r:notes.txt"}"""), new CallInfo())).Message);
        Assert.Equal("no such file: r:gone.cs", Assert.Throws<ToolException>(() => tool.Run(JsonNode.Parse("""{"path":"r:gone.cs"}"""), new CallInfo())).Message);
    }

    [Fact]
    // With `text`, the tool views that C# instead of the file, keeping the text's line numbers; the path only names it.
    public void test_view_tool_text()
    {
        using var repo = new TempRepo(new Dictionary<string, string> { ["Pages/Index.razor"] = "<h1>x</h1>\n" });
        var tool = new ViewTool(new RootSet([repo.Root()]));
        var args = new JsonObject { ["path"] = "r:Pages/Index.razor", ["text"] = "\n\nclass __RazorCode {\n    private void Load() { _logger.LogInformation(\"x\"); }\n    public int Count;\n}\n" };
        var text = tool.Run(args, new CallInfo());
        Assert.StartsWith("r:Pages/Index.razor 6 lines; terse: ", text);
        Assert.Contains("\n5~\t    int Count;", text);
        Assert.Equal("path is outside the root: ../x.razor", Assert.Throws<ToolException>(() => tool.Run(new JsonObject { ["path"] = "r:../x.razor", ["text"] = "" }, new CallInfo())).Message);
    }

    [Theory]
    // Long names become the initials of their words, or three letters of one word; short names stay.
    [InlineData("invoiceRepository", "ir")]
    [InlineData("customers", "cus")]
    [InlineData("HTTPClientFactory", "hcf")]
    [InlineData("total", null)]
    [InlineData("item2Count", "ic")]
    public void test_view_abbreviate(string name, string? expected) => Assert.Equal(expected, TerseView.Abbreviate(name));

    [Fact]
    // Only the local's own uses change: a field, a member and a parameter with the same name stay, captures in lambdas follow.
    public void test_view_shortens_locals_by_binding()
    {
        var v = TerseView.Format(TerseView.Build("""
            class C
            {
                int invoiceCount;
                int M(int customers, Other other)
                {
                    int invoiceCount = this.invoiceCount + other.invoiceCount;
                    foreach (var lineItem in Items()) { invoiceCount += lineItem; }
                    Func<int> f = () => invoiceCount + customers;
                    if (Try(out var resultValue)) return resultValue;
                    try { } catch (Exception exception) { Log(exception); }
                    return f();
                }
            }
            """).Lines);
        Assert.Contains("int ic = this.invoiceCount + other.invoiceCount;", v);
        Assert.Contains("foreach (var li in Items()) { ic += li; }", v);
        Assert.Contains("Func<int> f = () => ic + customers;", v);
        Assert.Contains("if (Try(out var rv)) return rv;", v);
        Assert.Contains("catch (Exception exc) { Log(exc); }", v);
        Assert.Contains("int invoiceCount;", v);
        Assert.Contains("M(int customers, Other other)", v);
    }

    [Fact]
    // A short name already used in the member, or a keyword, gets a number; a local used in nameof keeps its name.
    public void test_view_shortens_locals_without_clashes()
    {
        var v = TerseView.Format(TerseView.Build("""
            class C
            {
                void M()
                {
                    var ic = 1;
                    var invoiceCount = ic;
                    var integerValue = 2;
                    var asyncState = 3;
                    var renamedLater = 4;
                    Use(nameof(renamedLater), integerValue, asyncState);
                }
            }
            """).Lines);
        Assert.Contains("var ic2 = ic;", v);
        Assert.Contains("var iv = 2;", v);
        Assert.Contains("var as2 = 3;", v);
        Assert.Contains("var renamedLater = 4;", v);
    }

    private static string LongString(int n) => new string('x', n);

    [Fact]
    // A multi-line doc comment folds to its first summary sentence in summary mode, vanishes in none mode and stays in full mode.
    public void test_view_doc_modes()
    {
        const string src = "class C\n{\n    /// <summary>\n    /// Does the <see cref=\"Run\"/> thing. It also does more.\n    /// </summary>\n    /// <param name=\"x\">The x.</param>\n    /// <returns>Nothing.</returns>\n    void Run(int x) { }\n}\n";
        var summary = TerseView.Format(TerseView.Build(src, DocMode.Summary).Lines);
        Assert.Contains("3-7\t    /// Does the Run thing.\n8\t    void Run(int x) { }", summary);
        Assert.DoesNotContain("param", summary);
        var none = TerseView.Format(TerseView.Build(src, DocMode.None).Lines);
        Assert.DoesNotContain("///", none);
        Assert.Contains("8\t    void Run(int x) { }", none);
        Assert.Contains("<param name", TerseView.Format(TerseView.Build(src, DocMode.Full).Lines));
        Assert.Equal(DocMode.None, TerseView.ParseDocMode(" None "));
        Assert.Null(TerseView.ParseDocMode("x"));
    }

    [Fact]
    // A one-line doc and an <inheritdoc/> are left alone in summary mode, and // banners are hidden.
    public void test_view_docs_leave_short_ones_and_hide_banners()
    {
        const string src = "class C\n{\n    /// <summary>Short.</summary>\n    int A;\n    /// <inheritdoc/>\n    int B;\n    // ------------\n    // Section\n    int D;\n}\n";
        var v = TerseView.Format(TerseView.Build(src, DocMode.Summary).Lines);
        Assert.Contains("3~\t    /// Short.", v);
        Assert.Contains("5\t    /// <inheritdoc/>", v);
        Assert.DoesNotContain("------", v);
        Assert.Contains("8\t    // Section", v);
    }

    [Fact]
    // Very long strings, long collection initialisers and bodies of generated code fold, keeping the code around them.
    public void test_view_folds_literals_and_generated()
    {
        var items = string.Join(",\n        ", Enumerable.Range(1, 10).Select(i => $"\"item{i}\""));
        var src = $"class C\n{{\n    string P = \"{LongString(300)}\";\n    string[] L =\n    {{\n        {items}\n    }};\n    int Short = 1;\n}}\n";
        var v = TerseView.Format(TerseView.Build(src).Lines);
        Assert.Contains("3~\t    string P = \"…\"(302 chars);", v);
        Assert.Contains("5-16\t    { 10 items };", v);
        Assert.Contains("\t    int Short = 1;", v);
        Assert.DoesNotContain("item3", v);
        var gen = "// <auto-generated>\n// by a tool\n// </auto-generated>\nclass G\n{\n    void M()\n    {\n        a();\n        b();\n        c();\n    }\n}\n";
        var g = TerseView.Format(TerseView.Build(gen).Lines);
        Assert.Contains("7-11\t    { … 5 lines }", g);
        Assert.DoesNotContain("a();", g);
    }

    [Fact]
    // The tool refuses a view that saves under 3% (so the caller reads the source), unless forced, and logs sizes.
    public void test_view_tool_refuses_no_gain()
    {
        using var repo = new TempRepo(new Dictionary<string, string> { ["P.cs"] = "class C\n{\n    int X() => 1;\n}\n" });
        var tool = new ViewTool(new RootSet([repo.Root()]));
        var ex = Assert.Throws<ToolException>(() => tool.Run(JsonNode.Parse("""{"path":"r:P.cs"}"""), new CallInfo()));
        Assert.Equal("no_gain", ex.Code);
        var info = new CallInfo();
        Assert.StartsWith("r:P.cs 4 lines; terse: nothing to shorten, saves 0%", tool.Run(JsonNode.Parse("""{"path":"r:P.cs","force":true}"""), info));
        Assert.True(info.SourceBytes > 0 && info.ViewBytes > 0);
    }
}
