// Tests for --rename: references follow, files and documents move, and unsafe renames are refused.
using Xunit;

namespace ZazzleMinifier.Tests;

// Runs renames on throwaway projects.
public sealed class RenameTests : IDisposable
{
    private readonly string root = Directory.CreateTempSubdirectory("zazzle-csharp-minifier-rename").FullName;

    // Starts every test with a net8 project named App.
    public RenameTests() => Put("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings></PropertyGroup></Project>\n");

    // Deletes the throwaway folder.
    public void Dispose() => Directory.Delete(root, true);

    // Writes a file under the root.
    private void Put(string rel, string text)
    {
        var path = Path.Combine(root, rel);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, text);
    }

    // Reads a file under the root, or null when it is missing.
    private string? Read(string rel) => File.Exists(Path.Combine(root, rel)) ? File.ReadAllText(Path.Combine(root, rel)) : null;

    // Applies a rename file and returns the plan.
    private Plan Run(string renames)
    {
        Put("renames.txt", renames);
        var plan = Rename.Make(root, Path.Combine(root, "renames.txt"));
        plan.Apply();
        return plan;
    }

    [Fact]
    // A method rename reaches call sites in other files.
    public void test_rename_method_and_callers()
    {
        Put("Store.cs", "namespace App;\npublic class Store\n{\n    public void Add(int a) { }\n    public void Add(string s) { }\n}\n");
        Put("Use.cs", "namespace App;\npublic class Use\n{\n    public void Go(Store s) { s.Add(1); s.Add(\"x\"); }\n}\n");
        var plan = Run("App:\n    Store:\n        Add => Insert\n");
        Assert.Empty(plan.Errors);
        Assert.Contains("renamed App.Store.Add => Insert (2 files)", plan.Changes);
        Assert.Contains("public void Insert(int a)", Read("Store.cs"));
        Assert.Contains("public void Insert(string s)", Read("Store.cs"));
        Assert.Contains("s.Insert(1); s.Insert(\"x\");", Read("Use.cs"));
    }

    [Fact]
    // A type rename renames Type.cs, its src/ half, its comment document and its test folder, and fixes the document headings.
    public void test_rename_type_moves_files()
    {
        Put("Foo.cs", "namespace App;\npublic partial class Foo\n{\n    public partial int Get();\n}\n");
        Put("src/Foo.cs", "namespace App;\npublic partial class Foo\n{\n    public partial int Get() => 1;\n}\n");
        Put("document/Foo.cs.md", "# Foo.cs\n\n## class `App.Foo`\n\nA foo.\n\n## method `App.Foo.Get()`\n\nGets.\n");
        Put("tests/Foo/Get.cs", "namespace App.Tests;\npublic class FooTests\n{\n    [Fact]\n    public void test_get() { var f = new App.Foo(); _ = f.Get(); }\n}\n");
        var plan = Run("App:\n    Foo => Bar\n");
        Assert.Empty(plan.Errors);
        Assert.Null(Read("Foo.cs"));
        Assert.Contains("public partial class Bar", Read("Bar.cs"));
        Assert.Contains("public partial class Bar", Read("src/Bar.cs"));
        Assert.Equal("# Foo.cs\n\n## class `App.Bar`\n\nA foo.\n\n## method `App.Bar.Get()`\n\nGets.\n", Read("document/Bar.cs.md"));
        Assert.Contains("new App.Bar()", Read("tests/Bar/Get.cs"));
        Assert.False(Directory.Exists(Path.Combine(root, "tests/Foo")));
    }

    [Fact]
    // A member rename renames its <ClassUnderTest>/<Member>.cs test file.
    public void test_rename_member_moves_test_file()
    {
        Put("Store.cs", "namespace App;\npublic class Store { public int Add() => 1; }\n");
        Put("tests/Store/Add.cs", "namespace App.Tests;\npublic partial class StoreTests\n{\n    [Fact]\n    public void test_add() { _ = new App.Store().Add(); }\n}\n");
        var plan = Run("App:\n    Store:\n        Add => Plus\n");
        Assert.Empty(plan.Errors);
        Assert.Contains("new App.Store().Plus()", Read("tests/Store/Plus.cs"));
        Assert.Null(Read("tests/Store/Add.cs"));
    }

    [Fact]
    // A namespace rename moves its folder (with split src/ halves and sub-folders) and updates usings elsewhere.
    public void test_rename_namespace_moves_folder()
    {
        Put("Widgets/Knob.cs", "namespace App.Widgets;\npublic partial class Knob { public partial int Turn(); }\n");
        Put("Widgets/src/Knob.cs", "namespace App.Widgets;\npublic partial class Knob { public partial int Turn() => 1; }\n");
        Put("Widgets/Small/Dot.cs", "namespace App.Widgets.Small;\npublic class Dot { }\n");
        Put("document/Widgets/Knob.cs.md", "# Widgets/Knob.cs\n\n## class `App.Widgets.Knob`\n\nA knob.\n");
        Put("Use.cs", "using App.Widgets;\nusing App.Widgets.Small;\nnamespace App;\npublic class Use { public int Go(Knob k, Dot d) => k.Turn(); }\n");
        var plan = Run("App:\n    Widgets => Gadgets\n");
        Assert.Empty(plan.Errors);
        Assert.StartsWith("namespace App.Gadgets;", Read("Gadgets/Knob.cs"));
        Assert.StartsWith("namespace App.Gadgets;", Read("Gadgets/src/Knob.cs"));
        Assert.StartsWith("namespace App.Gadgets.Small;", Read("Gadgets/Small/Dot.cs"));
        Assert.Contains("## class `App.Gadgets.Knob`", Read("document/Gadgets/Knob.cs.md"));
        Assert.StartsWith("using App.Gadgets;\nusing App.Gadgets.Small;", Read("Use.cs"));
        Assert.False(Directory.Exists(Path.Combine(root, "Widgets")));
    }

    [Fact]
    // In a restructured project a namespace rename moves the folder in incl/ and src/ and its document in doc/.
    public void test_rename_namespace_restructured()
    {
        Put("App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework><EnableDefaultCompileItems>false</EnableDefaultCompileItems></PropertyGroup><ItemGroup><Compile Include=\"incl/**/*.cs\" /><Compile Include=\"src/**/*.cs\" /></ItemGroup></Project>\n");
        Put("incl/Widgets/Knob.cs", "namespace App.Widgets;\npublic partial class Knob { public partial int Turn(); }\n");
        Put("src/Widgets/Knob.cs", "namespace App.Widgets;\npublic partial class Knob { public partial int Turn() => 1; }\n");
        Put("doc/Widgets/Knob.cs.md", "# incl/Widgets/Knob.cs\n\n## class `App.Widgets.Knob`\n\nA knob.\n");
        var plan = Run("App:\n    Widgets => Gadgets\n");
        Assert.Empty(plan.Errors);
        Assert.StartsWith("namespace App.Gadgets;", Read("incl/Gadgets/Knob.cs"));
        Assert.StartsWith("namespace App.Gadgets;", Read("src/Gadgets/Knob.cs"));
        Assert.Contains("## class `App.Gadgets.Knob`", Read("doc/Gadgets/Knob.cs.md"));
        Assert.Null(Read("doc/Widgets/Knob.cs.md"));
    }

    [Fact]
    // A member, its type and its namespace renamed in one file, applied bottom-up.
    public void test_rename_bottom_up()
    {
        Put("Widgets/Knob.cs", "namespace App.Widgets;\npublic class Knob { public int Turn() => 1; }\n");
        Put("Use.cs", "namespace App;\npublic class Use { public int Go() => new App.Widgets.Knob().Turn(); }\n");
        var plan = Run("App:\n    Widgets => Gadgets:\n        Knob => Dial:\n            Turn => Spin\n");
        Assert.Empty(plan.Errors);
        Assert.Equal("namespace App.Gadgets;\npublic class Dial { public int Spin() => 1; }\n", Read("Gadgets/Dial.cs"));
        Assert.Contains("new App.Gadgets.Dial().Spin()", Read("Use.cs"));
    }

    [Fact]
    // An unknown path is an error naming its line, and nothing changes.
    public void test_rename_unknown_path()
    {
        Put("Store.cs", "namespace App;\npublic class Store { }\n");
        var plan = Run("App:\n    Shop => Market\n");
        Assert.Equal(["line 2: App.Shop is not declared in the source"], plan.Errors);
        Assert.Empty(plan.Edits);
    }

    [Fact]
    // An invalid new name is an error and nothing changes.
    public void test_rename_invalid_name()
    {
        Put("Store.cs", "namespace App;\npublic class Store { }\n");
        var plan = Run("App:\n    Store => int\n");
        Assert.Equal(["line 2: 'int' is not a valid C# identifier"], plan.Errors);
        Assert.Empty(plan.Edits);
    }

    [Fact]
    // A rename that would make two members collide is refused and nothing changes.
    public void test_rename_refuses_new_errors()
    {
        var text = "namespace App;\npublic class Store\n{\n    public void Add(int a) { }\n    public void Put(int a) { }\n}\n";
        Put("Store.cs", text);
        var plan = Run("App:\n    Store:\n        Add => Put\n");
        Assert.Single(plan.Errors);
        Assert.StartsWith("renames left unchanged: they would add compiler errors: App.csproj:CS0111", plan.Errors[0]);
        Assert.Empty(plan.Edits);
        Assert.Equal(text, Read("Store.cs"));
    }

    [Fact]
    // A move onto an existing file is refused.
    public void test_rename_refuses_collision()
    {
        Put("Foo.cs", "namespace App;\npublic class Foo { }\n");
        Put("Bar.cs", "namespace Other;\npublic class Bar { }\n");
        var plan = Run("App:\n    Foo => Bar\n");
        Assert.Equal(["renames left unchanged: Bar.cs already exists"], plan.Errors);
        Assert.Empty(plan.Edits);
    }
}
