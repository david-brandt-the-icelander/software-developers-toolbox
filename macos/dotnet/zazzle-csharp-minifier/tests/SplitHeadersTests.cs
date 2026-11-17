// Tests for --split-headers: which members split under each C# version, what each half looks like, and that the halves compile.
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Xunit;

namespace ZazzleMinifier.Tests;

// Runs the header split on throwaway projects and checks the files it leaves.
public sealed class SplitHeadersTests : IDisposable
{
    private readonly string root = Directory.CreateTempSubdirectory("zazzle-csharp-minifier-headers").FullName;

    // Deletes the throwaway folder.
    public void Dispose() => Directory.Delete(root, true);

    // Writes a file under the root and returns its full path.
    private string Put(string rel, string text)
    {
        var path = Path.Combine(root, rel);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, text);
        return path;
    }

    // Writes a minimal project file targeting the framework into the folder.
    private void Project(string dir, string framework) =>
        Put($"{dir}/{dir}.csproj", $"<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>{framework}</TargetFramework></PropertyGroup></Project>\n");

    // Plans and applies the split for the whole root.
    private Plan Run()
    {
        var plan = SplitHeaders.Make(root, SourceFiles.Under([root]).Where(f => !SourceFile.IsImplementationHalf(f)).ToList());
        plan.Apply();
        return plan;
    }

    // Reads a file under the root, or null when it does not exist.
    private string? Read(string rel) => File.Exists(Path.Combine(root, rel)) ? File.ReadAllText(Path.Combine(root, rel)) : null;

    // The compile errors of the files compiled together at the version against the .NET 8 reference assemblies.
    private static List<string> CompileErrors(LanguageVersion version, params string[] texts)
    {
        var options = CSharpParseOptions.Default.WithLanguageVersion(version);
        var trees = texts.Append("global using System; global using System.Threading; global using System.Threading.Tasks;").Select(t => CSharpSyntaxTree.ParseText(t, options));
        var compilation = CSharpCompilation.Create("t", trees, Basic.Reference.Assemblies.Net80.References.All,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, nullableContextOptions: NullableContextOptions.Enable));
        return compilation.GetDiagnostics().Where(d => d.Severity == DiagnosticSeverity.Error).Select(d => d.ToString()).ToList();
    }

    [Fact]
    // Under .NET 8 only methods split; defaults stay in the header, async moves to the implementation, and both halves compile.
    public void test_split_net8_methods_only()
    {
        Project("p8", "net8.0");
        Put("p8/Store.cs", """
            using System.Text;

            namespace App.Widgets;

            // A store.
            public class Store<T> where T : class
            {
                private int count;

                public int Count => count;

                // Adds.
                [Obsolete("x")]
                public void Add(T item, int times = 1)
                {
                    // inside
                    count += times;
                }

                public async Task<int> LoadAsync(CancellationToken token = default)
                {
                    await Task.Delay(1, token);
                    return count;
                }

                static U Pick<U>(U a) where U : struct => a;
            }

            """);
        var plan = Run();
        Assert.Empty(plan.Errors);
        Assert.Equal(["p8/Store.cs: moved 3 bodies to p8/src/Store.cs"], plan.Changes);
        Assert.Equal("""
            using System.Text;

            namespace App.Widgets;

            // A store.
            public partial class Store<T> where T : class
            {
                private int count;

                public int Count => count;

                // Adds.
                [Obsolete("x")]
                public partial void Add(T item, int times = 1);

                public partial Task<int> LoadAsync(CancellationToken token = default);

                private static partial U Pick<U>(U a) where U : struct;
            }

            """, Read("p8/Store.cs"));
        Assert.Equal("""
            using System.Text;

            namespace App.Widgets;

            public partial class Store<T>
            {
                public partial void Add(T item, int times)
                {
                    // inside
                    count += times;
                }

                public async partial Task<int> LoadAsync(CancellationToken token)
                {
                    await Task.Delay(1, token);
                    return count;
                }

                private static partial U Pick<U>(U a) where U : struct => a;
            }

            """, Read("p8/src/Store.cs"));
        Assert.Empty(CompileErrors(LanguageVersion.CSharp12, Read("p8/Store.cs")!, Read("p8/src/Store.cs")!));
    }

    [Fact]
    // Under .NET 9 computed properties and indexers split too, but auto-properties stay.
    public void test_split_net9_properties_and_indexers()
    {
        Project("p9", "net9.0");
        Put("p9/A.cs", "public class A\n{\n    public int V { get; set; }\n    public int Twice { get => V * 2; private set => V = value / 2; }\n    public int this[int i] => i + V;\n}\n");
        var plan = Run();
        Assert.Empty(plan.Errors);
        Assert.Equal("public partial class A\n{\n    public int V { get; set; }\n    public partial int Twice { get; private set; }\n    public partial int this[int i] { get; }\n}\n", Read("p9/A.cs"));
        Assert.Equal("public partial class A\n{\n    public partial int Twice { get => V * 2; private set => V = value / 2; }\n\n    public partial int this[int i] => i + V;\n}\n", Read("p9/src/A.cs"));
        Assert.Empty(CompileErrors(LanguageVersion.CSharp13, Read("p9/A.cs")!, Read("p9/src/A.cs")!));
    }

    [Fact]
    // Under .NET 10 constructors (with their base call) and events with accessors split too, in a block namespace.
    public void test_split_net10_constructors_and_events()
    {
        Project("p10", "net10.0");
        Put("p10/Thing.cs", """
            namespace App
            {
                public class Base(int x) { }

                public class Thing : Base
                {
                    private Action? handler;

                    public Thing(int x) : base(x)
                    {
                        handler = null;
                    }

                    public event Action Changed
                    {
                        add { handler += value; }
                        remove { handler -= value; }
                    }
                }
            }

            """);
        var plan = Run();
        Assert.Empty(plan.Errors);
        Assert.Contains("public partial Thing(int x);", Read("p10/Thing.cs"));
        Assert.Contains("public partial event Action Changed;", Read("p10/Thing.cs"));
        Assert.Contains("public class Base(int x) { }", Read("p10/Thing.cs"));
        Assert.Equal("""
            namespace App
            {
                public partial class Thing
                {
                    public partial Thing(int x) : base(x)
                    {
                        handler = null;
                    }

                    public partial event Action Changed
                    {
                        add { handler += value; }
                        remove { handler -= value; }
                    }
                }
            }

            """, Read("p10/src/Thing.cs"));
        Assert.Empty(CompileErrors(LanguageVersion.CSharp14, Read("p10/Thing.cs")!, Read("p10/src/Thing.cs")!));
    }

    [Fact]
    // Nested types, static classes, extension methods and records keep their shape in both halves.
    public void test_split_nested_static_extension_record()
    {
        Project("p", "net8.0");
        Put("p/N.cs", """
            namespace N;

            public static class Ext
            {
                public static int Twice(this int x, params int[] more) => x * 2;
            }

            public class Outer
            {
                public class Inner
                {
                    internal string Name() => "inner";
                }
            }

            public record Point(int X)
            {
                public int Double() => X * 2;
            }

            public record struct Pair(int A)
            {
                public readonly int Sum() => A;
            }

            """);
        var plan = Run();
        Assert.Empty(plan.Errors);
        var impl = Read("p/src/N.cs")!;
        Assert.Contains("public static partial class Ext", impl);
        Assert.Contains("public static partial int Twice(this int x, params int[] more) => x * 2;", impl);
        Assert.Contains("public partial class Outer\n{\n    public partial class Inner\n    {\n        internal partial string Name() => \"inner\";", impl);
        Assert.Contains("public partial record Point\n", impl);
        Assert.Contains("public partial record struct Pair\n", impl);
        Assert.Contains("public partial record Point(int X)", Read("p/N.cs"));
        Assert.Empty(CompileErrors(LanguageVersion.CSharp12, Read("p/N.cs")!, impl));
    }

    [Fact]
    // Types defined elsewhere in the project do not stop the split, since the check only looks for new errors.
    public void test_split_unknown_project_types()
    {
        Project("p", "net8.0");
        Put("p/A.cs", "class A\n{\n    Widget Make(Gadget g) => new Widget(g);\n}\n");
        var plan = Run();
        Assert.Empty(plan.Errors);
        Assert.Equal("partial class A\n{\n    private partial Widget Make(Gadget g);\n}\n", Read("p/A.cs"));
    }

    [Fact]
    // A second run on the split header changes nothing.
    public void test_split_second_run_no_edits()
    {
        Project("p", "net8.0");
        Put("p/A.cs", "class A\n{\n    int F() => 1;\n}\n");
        Run();
        var again = Run();
        Assert.Empty(again.Edits);
        Assert.Equal("partial class A\n{\n    private partial int F();\n}\n", Read("p/A.cs"));
    }

    [Fact]
    // A body added to the header later is moved into the existing implementation file.
    public void test_split_merges_into_existing_half()
    {
        Project("p", "net8.0");
        Put("p/A.cs", "using System.Text;\n\nclass A\n{\n    int F() => 1;\n}\n");
        Run();
        Put("p/A.cs", "using System.Text;\nusing System.IO;\n\npartial class A\n{\n    private partial int F();\n    int G() => 2;\n}\n");
        var plan = Run();
        Assert.Empty(plan.Errors);
        Assert.Equal("using System.Text;\nusing System.IO;\n\npartial class A\n{\n    private partial int F() => 1;\n\n    private partial int G() => 2;\n}\n", Read("p/src/A.cs"));
        Assert.Empty(CompileErrors(LanguageVersion.CSharp12, Read("p/A.cs")!, Read("p/src/A.cs")!));
    }

    [Fact]
    // Files with top-level statements, directives inside types, old frameworks or no project are left whole with a note.
    public void test_split_skips()
    {
        Project("p", "net8.0");
        Put("p/Program.cs", "Console.WriteLine(1);\nint F() => 1;\n");
        Put("p/D.cs", "class D\n{\n#if !DEBUG\n    int F() => 1;\n#endif\n}\n");
        Project("old", "netstandard2.0");
        Put("old/O.cs", "class O { int F() => 1; }\n");
        Put("loose/L.cs", "class L { int F() => 1; }\n");
        var plan = Run();
        Assert.Empty(plan.Edits);
        Assert.Contains("p/Program.cs: skipped, top-level statements", plan.Notes);
        Assert.Contains("p/D.cs: skipped, preprocessor directive #if !DEBUG inside a type", plan.Notes);
        Assert.Contains("old/O.cs: skipped, C# 9 or later needed", plan.Notes);
        Assert.Contains("loose/L.cs: skipped, no .csproj found", plan.Notes);
    }

    [Fact]
    // Regions stay in the header, directives inside a moved body move with it, and #nullable at file level is copied.
    public void test_split_regions_and_body_directives()
    {
        Project("p", "net8.0");
        Put("p/R.cs", "#nullable enable\nclass R\n{\n    #region Work\n    int F()\n    {\n#if DEBUG\n        return 1;\n#else\n        return 2;\n#endif\n    }\n    #endregion\n}\n");
        var plan = Run();
        Assert.Empty(plan.Errors);
        Assert.Equal("#nullable enable\npartial class R\n{\n    #region Work\n    private partial int F();\n    #endregion\n}\n", Read("p/R.cs"));
        Assert.StartsWith("#nullable enable\n\npartial class R\n", Read("p/src/R.cs"));
        Assert.Contains("#if DEBUG\n        return 1;", Read("p/src/R.cs"));
    }

    [Fact]
    // LangVersion in the project, or in a Directory.Build.props above it, wins over the target framework.
    public void test_project_language_lang_version()
    {
        Put("Directory.Build.props", "<Project><PropertyGroup><LangVersion>13</LangVersion></PropertyGroup></Project>\n");
        Project("p", "net8.0");
        var file = Put("p/A.cs", "class A { }\n");
        Assert.Equal(LanguageVersion.CSharp13, ProjectLanguage.For(root, file).Version);
        Put("p/p.csproj", "<Project><PropertyGroup><TargetFramework>net8.0</TargetFramework><LangVersion>latest</LangVersion></PropertyGroup></Project>\n");
        Assert.True(ProjectLanguage.For(root, file).Version >= LanguageVersion.CSharp14);
        Put("p/p.csproj", "<Project><PropertyGroup><TargetFrameworks>net9.0;net8.0</TargetFrameworks></PropertyGroup></Project>\n");
        File.Delete(Path.Combine(root, "Directory.Build.props"));
        Assert.Equal(LanguageVersion.CSharp13, ProjectLanguage.For(root, file).Version);
    }

    [Fact]
    // CRLF line endings and a byte-order mark carry over to both halves.
    public void test_split_keeps_crlf_and_bom()
    {
        Project("p", "net8.0");
        var path = Path.Combine(root, "p", "A.cs");
        File.WriteAllBytes(path, [0xEF, 0xBB, 0xBF, .. System.Text.Encoding.UTF8.GetBytes("class A\r\n{\r\n    int F()\r\n    {\r\n        return 1;\r\n    }\r\n}\r\n")]);
        Run();
        var impl = File.ReadAllBytes(Path.Combine(root, "p", "src", "A.cs"));
        Assert.Equal([0xEF, 0xBB, 0xBF], impl[..3]);
        Assert.Equal("partial class A\r\n{\r\n    private partial int F()\r\n    {\r\n        return 1;\r\n    }\r\n}\r\n", System.Text.Encoding.UTF8.GetString(impl[3..]));
        Assert.Equal("partial class A\r\n{\r\n    private partial int F();\r\n}\r\n", System.Text.Encoding.UTF8.GetString(File.ReadAllBytes(path)[3..]));
    }
}
