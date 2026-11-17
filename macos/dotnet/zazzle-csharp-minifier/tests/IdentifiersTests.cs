// Tests for --list-identifiers and the rename-file parser.
using Xunit;

namespace ZazzleMinifier.Tests;

// Lists identifiers from throwaway folders and parses rename files.
public sealed class IdentifiersTests : IDisposable
{
    private readonly string root = Directory.CreateTempSubdirectory("zazzle-csharp-minifier-ids").FullName;

    // Deletes the throwaway folder.
    public void Dispose() => Directory.Delete(root, true);

    // Writes a file under the root.
    private void Put(string rel, string text)
    {
        var path = Path.Combine(root, rel);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, text);
    }

    // The listing for the whole root.
    private string List() => Identifiers.List(root, SourceFiles.Under([root]));

    [Fact]
    // Dotted namespaces nest a level per part, and the user's example prints as given.
    public void test_list_nested_namespaces()
    {
        Put("a.cs", "namespace foo.bar { class c { } }\n");
        Assert.Equal("foo:\n    bar:\n        c\n", List());
    }

    [Fact]
    // Nested types, members, enum members, overloads collapsed, constructors and indexers left out, block and file-scoped namespaces merged.
    public void test_list_members()
    {
        Put("a.cs", """
            namespace App;
            public class Store<T>
            {
                public Store() { }
                private int count, total;
                public const int Max = 3;
                public void Add(int a) { }
                public void Add(string a) { }
                public int this[int i] => i;
                public event System.Action? Changed;
                public int Size => count;
                public class Inner { void Go() { } }
            }
            """);
        Put("b.cs", "namespace App { enum Colour { Red, Green } delegate void Handler(); }\nclass Global { }\n");
        Assert.Equal("""
            App:
                Colour:
                    Green
                    Red
                Handler
                Store:
                    Add
                    Changed
                    Inner:
                        Go
                    Max
                    Size
                    count
                    total
            Global

            """, List());
    }

    [Fact]
    // Generated files are left out of the listing.
    public void test_list_skips_generated()
    {
        Put("a.g.cs", "namespace Gen { class G { } }\n");
        Assert.Equal("", List());
    }

    [Fact]
    // A rename file gives requests deepest first, with their lines; plain lines only set the path.
    public void test_parse_rename_file()
    {
        var (renames, errors) = IdentifierTree.Parse("# renames\nfoo => fu:\n    bar:\n        c => fubar\n    other\n");
        Assert.Empty(errors);
        Assert.Equal(["foo.bar.c => fubar @4", "foo => fu @2"], renames.Select(r => $"{r.Dotted} => {r.NewName} @{r.Line}"));
    }

    [Fact]
    // A keyword or malformed new name is an error with its line.
    public void test_parse_rename_file_errors()
    {
        var (_, errors) = IdentifierTree.Parse("foo:\n    c => class\n    d => 1x\n");
        Assert.Equal(["line 2: 'class' is not a valid C# identifier", "line 3: '1x' is not a valid C# identifier"], errors);
    }
}
