// Tests for the terse C# view and its MCP tool.
using System.Text.Json.Nodes;
using ZazzleIndexer.Tools;
using ZazzleIndexer.View;
using Xunit;

namespace ZazzleIndexer.Tests;

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
        Assert.Contains("~\t        var invoice = (Invoice)_repo.Get(id);", v);
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
        var text = tool.Run(JsonNode.Parse("""{"path":"r:src/S.cs"}"""), new CallInfo());
        Assert.Matches(@"^r:src/S.cs \d+ lines; terse: 1 usings, 1 constructors, 1 log-and-rethrow catches, ", text);
        Assert.Contains("\n1-2\t⋯ 2 using(s)\n", text);
        Assert.Equal("not a C# file: r:notes.txt", Assert.Throws<ToolException>(() => tool.Run(JsonNode.Parse("""{"path":"r:notes.txt"}"""), new CallInfo())).Message);
        Assert.Equal("no such file: r:gone.cs", Assert.Throws<ToolException>(() => tool.Run(JsonNode.Parse("""{"path":"r:gone.cs"}"""), new CallInfo())).Message);
    }
}
