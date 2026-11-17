// Tests for --restructure: a small solution turned into <project>/incl, src, doc and test.
using Xunit;

namespace ZazzleMinifier.Tests;

// Runs --restructure on a throwaway git repository holding a library, its test project and a solution.
[System.Runtime.Versioning.UnsupportedOSPlatform("windows")]
public sealed class RestructureTests : IDisposable
{
    private readonly string root = Directory.CreateTempSubdirectory("zazzle-csharp-minifier-restructure").FullName;

    // Writes the sample solution and makes the folder a git repository.
    public RestructureTests()
    {
        Proc.Run("git", ["-C", root, "init", "-q"], 20);
        Put("Lib/Lib.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n");
        Put("Lib/Foo.cs", "namespace Lib;\n\n// Adds numbers.\npublic class Foo\n{\n    // Returns the sum.\n    public int Add(int a, int b)\n    {\n        return a + b;\n    }\n}\n");
        Put("Lib/Models/Bar.cs", "namespace Lib.Models;\n\npublic enum Bar\n{\n    One,\n}\n");
        Put("Lib.Tests/Lib.Tests.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <IsTestProject>true</IsTestProject>\n  </PropertyGroup>\n  <ItemGroup>\n    <ProjectReference Include=\"..\\Lib\\Lib.csproj\" />\n  </ItemGroup>\n</Project>\n");
        Put("Lib.Tests/FooTests.cs", "using Xunit;\n\nnamespace Lib.Tests;\n\npublic class FooTests\n{\n    [Fact]\n    public void test_add_sums()\n    {\n        // Given\n        var foo = new Foo();\n        // When\n        var sum = foo.Add(1, 2);\n        // Then\n        Assert.Equal(3, sum);\n    }\n}\n");
        Put("App.sln", "Microsoft Visual Studio Solution File, Format Version 12.00\n"
            + "Project(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"Lib\", \"Lib\\Lib.csproj\", \"{11111111-1111-1111-1111-111111111111}\"\nEndProject\n"
            + "Project(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"Lib.Tests\", \"Lib.Tests\\Lib.Tests.csproj\", \"{22222222-2222-2222-2222-222222222222}\"\nEndProject\n");
    }

    // Deletes the repository.
    public void Dispose() => Directory.Delete(root, true);

    // Writes a file under the root.
    private void Put(string rel, string text)
    {
        var path = Path.Combine(root, rel);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, text);
    }

    // Reads a file under the root, or null when it does not exist.
    private string? Read(string rel) => File.Exists(Path.Combine(root, rel)) ? File.ReadAllText(Path.Combine(root, rel)) : null;

    // Runs the tool with the switches and returns its exit code and output.
    private (int Code, string Output) Run(params string[] args)
    {
        var output = new StringWriter();
        var errors = new StringWriter();
        var code = Runner.Run(Cli.Parse([.. args, "--root", root]), output, errors);
        return (code, output + errors.ToString());
    }

    [Fact]
    // The library ends up with incl/, src/, doc/ and test/, and the solution, project files, .ignore and AGENTS.md follow.
    public void test_restructure_solution()
    {
        var (code, output) = Run("--restructure");
        Assert.True(code == 0, output);
        Assert.Contains("Lib: restructured into incl/ (2), src/ (0), doc/ (1), test/ (3)", output);
        Assert.Contains("public partial int Add(int a, int b);", Read("Lib/incl/Foo.cs"));
        Assert.Contains("return a + b;", Read("Lib/src/Foo.cs"));
        Assert.Contains("enum Bar", Read("Lib/incl/Models/Bar.cs"));
        Assert.Contains("Returns the sum.", Read("Lib/doc/Foo.cs.md"));
        Assert.Contains("<ProjectReference Include=\"..\\Lib.csproj\" />", Read("Lib/test/Lib.Tests.csproj"));
        Assert.Contains("test_add_sums", Read("Lib/test/Foo/Add.cs"));
        Assert.Contains("\"Lib\\test\\Lib.Tests.csproj\"", Read("App.sln"));
        Assert.False(Directory.Exists(Path.Combine(root, "Lib.Tests")));
        Assert.False(Directory.Exists(Path.Combine(root, "document")));
        Assert.Equal(Restructure.IgnoreText, Read("Lib/.ignore"));
        var csproj = Read("Lib/Lib.csproj")!;
        Assert.Contains("<EnableDefaultCompileItems>false</EnableDefaultCompileItems>", csproj);
        Assert.Contains("<Compile Include=\"incl/**/*.cs\" />", csproj);
        Assert.Contains("<DefaultItemExcludes>$(DefaultItemExcludes);test/**</DefaultItemExcludes>", csproj);
        Assert.Contains("In the restructured projects (`Lib`), read `incl/` first and always", Read("AGENTS.md"));
        Assert.Contains("--strip-comments --split-tests --split-headers --restructure", Read(".githooks/pre-commit"));
    }

    [Fact]
    // A second run, and a check, find nothing more to do.
    public void test_restructure_fixpoint()
    {
        Run("--restructure");
        var (code, output) = Run("--check", "--restructure");
        Assert.True(code == 0, output);
        Assert.DoesNotContain("would change", output);
    }

    [Fact]
    // A new plain file in a restructured project is a violation, and the next run splits it into incl/ and src/.
    public void test_restructure_places_new_file()
    {
        Run("--restructure");
        Put("Lib/Baz.cs", "namespace Lib;\n\npublic class Baz\n{\n    public int Two() => 2;\n}\n");
        var (code, output) = Run("--check", "--restructure");
        Assert.Equal(1, code);
        Assert.Contains("--restructure would change Lib: restructured into incl/ (1)", output);
        Run("--restructure");
        Assert.Contains("public partial int Two();", Read("Lib/incl/Baz.cs"));
        Assert.Contains("=> 2;", Read("Lib/src/Baz.cs"));
        Assert.False(File.Exists(Path.Combine(root, "Lib/Baz.cs")));
        Assert.False(Directory.Exists(Path.Combine(root, "Lib/src/src")));
    }

    [Fact]
    // A top-level program goes to src/, and a file already in the way stops the project with an error.
    public void test_restructure_program_and_collision()
    {
        Put("Lib/Program.cs", "System.Console.WriteLine(1);\n");
        Put("Lib/doc/Foo.cs.md", "mine\n");
        var (code, output) = Run("--restructure");
        Assert.Equal(1, code);
        Assert.Contains("Lib: left unchanged:", output);
        Assert.Contains("is in the way of", output);
        File.Delete(Path.Combine(root, "Lib/doc/Foo.cs.md"));
        Run("--restructure");
        Assert.Equal("System.Console.WriteLine(1);\n", Read("Lib/src/Program.cs"));
    }

    [Fact]
    // Paths that climb out of a moved project file are re-pointed from its new folder, keeping their separator style.
    public void test_relocate_paths()
    {
        var text = "<ProjectReference Include=\"..\\Lib\\Lib.csproj\" /><None Include=\"../shared/a.json;local.txt\" /><HintPath>..\\libs\\x.dll</HintPath>";
        var moved = ProjectFile.Relocate(text, "/r/Lib.Tests", "/r/Lib/test");
        Assert.Equal("<ProjectReference Include=\"..\\Lib.csproj\" /><None Include=\"../../shared/a.json;local.txt\" /><HintPath>..\\..\\libs\\x.dll</HintPath>", moved);
    }

    [Fact]
    // An .slnx entry is re-pointed too.
    public void test_solution_repoint_slnx()
    {
        Put("All.slnx", "<Solution>\n  <Project Path=\"Lib.Tests/Lib.Tests.csproj\" />\n</Solution>\n");
        var text = SolutionFiles.Repoint(Path.Combine(root, "All.slnx"), Path.Combine(root, "Lib.Tests/Lib.Tests.csproj"), Path.Combine(root, "Lib/test/Lib.Tests.csproj"));
        Assert.Equal("<Solution>\n  <Project Path=\"Lib/test/Lib.Tests.csproj\" />\n</Solution>\n", text);
    }
}
