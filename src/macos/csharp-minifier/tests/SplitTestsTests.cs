// Tests for --split-tests: where tests go, what stays behind, and the marker on tests that cannot be placed.
using Xunit;

namespace CSharpMinifier.Tests;

// Runs the test split on throwaway folders that hold a small library and its tests.
public sealed class SplitTestsTests : IDisposable
{
    private readonly string root = Directory.CreateTempSubdirectory("csharp-minifier").FullName;

    // The library the tests exercise.
    private const string Library = """
        namespace Lib;

        public class Store
        {
            public Store() { }
            public Store(int size) { }
            public int Count => 0;
            public void Add(int x) { }
            public void Add(string x) { }
            public void Remove(int x) { }
        }

        public static class Parser
        {
            public static int Parse(string s) => 0;
        }

        """;

    // Writes the library under src/.
    public SplitTestsTests() => Put("src/Store.cs", Library);

    // Deletes the throwaway folder.
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

    // Plans the split over the whole root, applies it, and returns the plan.
    private Plan Run()
    {
        var plan = SplitTests.Make(root, SourceFiles.Under([root]));
        plan.Apply();
        return plan;
    }

    // A test class file with the given members in namespace Tests.
    private static string TestFile(string members, string header = "public class StoreTests") =>
        "using Lib;\nusing Xunit;\n\nnamespace Tests;\n\n" + header + "\n{\n" + members + "}\n";

    // A test method whose When section is the given statement.
    private static string Test(string name, string when) =>
        $"    [Fact]\n    public void {name}()\n    {{\n        var store = new Store();\n        // When\n        {when}\n        // Then\n        Assert.True(true);\n    }}\n";

    [Fact]
    // Tests move to one file per method under test inside a folder named after the class under test.
    public void test_split_resolved_into_folders()
    {
        Put("tests/StoreTests.cs", TestFile(Test("test_add_one", "store.Add(1);") + "\n" + Test("test_remove_one", "store.Remove(1);")));
        var plan = Run();
        Assert.Empty(plan.Errors);
        Assert.Equal(["tests/StoreTests.cs: moved 2 tests to tests/Store/Add.cs, tests/Store/Remove.cs"], plan.Changes);
        Assert.Null(Read("tests/StoreTests.cs"));
        Assert.Equal(TestFile(Test("test_add_one", "store.Add(1);"), "public partial class StoreTests"), Read("tests/Store/Add.cs"));
        Assert.Contains("public void test_remove_one()", Read("tests/Store/Remove.cs"));
    }

    [Fact]
    // Overloads of one method share a file, and a second run changes nothing.
    public void test_split_overloads_and_fixpoint()
    {
        Put("tests/StoreTests.cs", TestFile(Test("test_add_int", "store.Add(1);") + "\n" + Test("test_add_text", "store.Add(\"a\");")));
        Run();
        var add = Read("tests/Store/Add.cs")!;
        Assert.Contains("test_add_int", add);
        Assert.Contains("test_add_text", add);
        var again = Run();
        Assert.Empty(again.Edits);
        Assert.Empty(again.Changes);
    }

    [Fact]
    // A construction in the When section targets the constructor; a property read targets the property.
    public void test_split_constructor_and_property()
    {
        Put("tests/StoreTests.cs", TestFile(Test("test_new_sized", "var s = new Store(3);") + "\n" + Test("test_count_empty", "var n = store.Count;")));
        Run();
        Assert.Contains("test_new_sized", Read("tests/Store/Constructor.cs"));
        Assert.Contains("test_count_empty", Read("tests/Store/Count.cs"));
    }

    [Fact]
    // A call nested in another call's arguments is not the method under test.
    public void test_split_nested_call_uses_outer()
    {
        Put("tests/StoreTests.cs", TestFile(Test("test_add_parsed", "store.Add(Parser.Parse(\"1\"));")));
        Run();
        Assert.Contains("test_add_parsed", Read("tests/Store/Add.cs"));
    }

    [Fact]
    // Helpers declared in test files are never the code under test.
    public void test_split_ignores_test_helpers()
    {
        var helper = "    private static void Helper(Store s) => s.Add(1);\n\n";
        Put("tests/StoreTests.cs", TestFile(helper + Test("test_add_via_helper", "Helper(store); store.Add(2);")));
        Run();
        Assert.Contains("test_add_via_helper", Read("tests/Store/Add.cs"));
        Assert.Contains("Helper(Store s)", Read("tests/StoreTests.cs"));
    }

    [Fact]
    // A test without a When section stays and gets one marker, which a second run does not repeat.
    public void test_split_unresolved_marker_once()
    {
        var test = "    [Fact]\n    public void test_nothing()\n    {\n        Assert.True(true);\n    }\n";
        Put("tests/StoreTests.cs", TestFile(test));
        var plan = Run();
        Assert.Equal(["tests/StoreTests.cs: marked 1 tests whose method under test is unresolved"], plan.Changes);
        var marked = Read("tests/StoreTests.cs")!;
        Assert.Contains("    {\n        // csharp-minifier: unresolved method under test (no // When section)\n        Assert.True(true);", marked);
        Assert.Empty(Run().Changes);
        Assert.Equal(marked, Read("tests/StoreTests.cs"));
    }

    [Fact]
    // Calls to two different members in the When section leave the test marked with both names.
    public void test_split_several_members_marked()
    {
        Put("tests/StoreTests.cs", TestFile(Test("test_add_remove", "store.Add(1); store.Remove(1);")));
        Run();
        Assert.Contains("(// When calls several members: Store.Add, Store.Remove)", Read("tests/StoreTests.cs"));
    }

    [Fact]
    // Once a marked test resolves, it moves without its marker.
    public void test_split_marker_dropped_when_resolved()
    {
        var marked = "    [Fact]\n    public void test_add_one()\n    {\n        // csharp-minifier: unresolved method under test (no // When section)\n        var store = new Store();\n        // When\n        store.Add(1);\n    }\n";
        Put("tests/StoreTests.cs", TestFile(marked));
        Run();
        var add = Read("tests/Store/Add.cs")!;
        Assert.DoesNotContain("csharp-minifier", add);
        Assert.Contains("store.Add(1);", add);
    }

    [Fact]
    // Fields, constructors and class attributes stay in the original file, which becomes partial.
    public void test_split_shared_members_stay()
    {
        var shared = "    private readonly int seed = 1;\n\n";
        Put("tests/StoreTests.cs", TestFile(shared + Test("test_add_one", "store.Add(1);"), "[Collection(\"x\")]\npublic class StoreTests"));
        Run();
        Assert.Equal("using Lib;\nusing Xunit;\n\nnamespace Tests;\n\n[Collection(\"x\")]\npublic partial class StoreTests\n{\n    private readonly int seed = 1;\n}\n",
            Read("tests/StoreTests.cs"));
    }

    [Fact]
    // Two test classes testing the same method end up in one target file.
    public void test_split_merges_two_classes()
    {
        Put("tests/StoreTests.cs", TestFile(Test("test_add_one", "store.Add(1);")));
        Put("tests/MoreTests.cs", TestFile(Test("test_add_two", "store.Add(2);"), "public class MoreTests"));
        var plan = Run();
        Assert.Empty(plan.Errors);
        var add = Read("tests/Store/Add.cs")!;
        Assert.Contains("public partial class MoreTests", add);
        Assert.Contains("public partial class StoreTests", add);
        Assert.Contains("test_add_one", add);
        Assert.Contains("test_add_two", add);
    }

    [Fact]
    // A test added to an existing class-under-test folder joins the existing target file.
    public void test_split_extends_existing_target()
    {
        Put("tests/StoreTests.cs", TestFile(Test("test_add_one", "store.Add(1);")));
        Run();
        Put("tests/StoreTests.cs", TestFile(Test("test_add_two", "store.Add(2);")));
        Run();
        var add = Read("tests/Store/Add.cs")!;
        Assert.Contains("test_add_one", add);
        Assert.Contains("test_add_two", add);
        Assert.Single(System.Text.RegularExpressions.Regex.Matches(add, "partial class StoreTests"));
    }

    [Fact]
    // Block namespaces and CRLF line endings carry over to the new file.
    public void test_split_block_namespace_crlf()
    {
        var text = "using Lib;\r\nusing Xunit;\r\n\r\nnamespace Tests\r\n{\r\n    public class StoreTests\r\n    {\r\n        [Fact]\r\n        public void test_add_one()\r\n        {\r\n            var store = new Store();\r\n            // When\r\n            store.Add(1);\r\n        }\r\n    }\r\n}\r\n";
        Put("tests/StoreTests.cs", text);
        Run();
        Assert.Equal("using Lib;\r\nusing Xunit;\r\n\r\nnamespace Tests\r\n{\r\n    public partial class StoreTests\r\n    {\r\n        [Fact]\r\n        public void test_add_one()\r\n        {\r\n            var store = new Store();\r\n            // When\r\n            store.Add(1);\r\n        }\r\n    }\r\n}\r\n",
            Read("tests/Store/Add.cs"));
    }

    [Fact]
    // Files without tests and generated test files are left alone.
    public void test_split_skips_non_tests_and_generated()
    {
        Put("tests/Gen.g.cs", TestFile(Test("test_add_one", "store.Add(1);")));
        var plan = Run();
        Assert.Contains("tests/Gen.g.cs: skipped, generated code", plan.Notes);
        Assert.Empty(plan.Edits);
        Assert.Equal(Library, Read("src/Store.cs"));
    }
}
