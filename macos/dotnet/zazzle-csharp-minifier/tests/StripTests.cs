// Tests for --strip-comments: which comments move, what the code looks like after, and what the Markdown says.
using Xunit;

namespace ZazzleMinifier.Tests;

// Runs the strip command on throwaway folders and checks the files it leaves.
public sealed class StripTests : IDisposable
{
    private readonly string root = Directory.CreateTempSubdirectory("zazzle-csharp-minifier").FullName;

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

    // Runs the command on the whole root and returns the exit code and output.
    private (int Code, string Output, string Errors) Run()
    {
        var plan = StripComments.Make(root, SourceFiles.Under([root]));
        plan.Apply();
        var output = string.Join("\n", plan.Notes.Concat(plan.Changes));
        return (plan.Errors.Count > 0 ? 1 : 0, output, string.Join("\n", plan.Errors));
    }

    // Reads a file under the root, or null when it does not exist.
    private string? Read(string rel) => File.Exists(Path.Combine(root, rel)) ? File.ReadAllText(Path.Combine(root, rel)) : null;

    [Fact]
    // File, type, member, field, enum member and accessor comments move; body comments stay.
    public void test_strip_declarations_moved_bodies_kept()
    {
        Put("foo/bar.cs", """
            // Builds the index.
            using System;

            namespace App;

            // A store.
            public class Store
            {
                // How many.
                private int count; // the count

                // Adds one.
                [Obsolete]
                public void Add(int a)
                {
                    // inside stays
                    count++; // also stays
                }

                // The size.
                public int Size
                {
                    // getter
                    get => count;
                }
            }

            // Colours.
            public enum Colour
            {
                Red, // the red
                // green one
                Green,
            }

            """);
        var (code, output, _) = Run();
        Assert.Equal(0, code);
        Assert.Contains("foo/bar.cs: moved 10 comments to document/foo/bar.cs.md", output);
        Assert.Equal("""
            using System;

            namespace App;

            public class Store
            {
                private int count;

                [Obsolete]
                public void Add(int a)
                {
                    // inside stays
                    count++; // also stays
                }

                public int Size
                {
                    get => count;
                }
            }

            public enum Colour
            {
                Red,
                Green,
            }

            """, Read("foo/bar.cs"));
        Assert.Equal("""
            # foo/bar.cs

            ## File

            Builds the index.

            ## class `App.Store`

            A store.

            ## field `App.Store.count`

            How many.
            the count

            ## method `App.Store.Add(int)`

            Adds one.

            ## property `App.Store.Size`

            The size.

            ## accessor `App.Store.Size.get`

            getter

            ## enum `App.Colour`

            Colours.

            ## enum member `App.Colour.Red`

            the red

            ## enum member `App.Colour.Green`

            green one

            """, Read("document/foo/bar.cs.md"));
    }

    [Fact]
    // XML doc comments move whole, markers and all lines, into an xml block.
    public void test_strip_doc_comment()
    {
        Put("a.cs", "namespace A;\n\n/// <summary>\n/// Thing.\n/// </summary>\npublic class Thing { }\n");
        Run();
        Assert.Equal("namespace A;\n\npublic class Thing { }\n", Read("a.cs"));
        Assert.Contains("## class `A.Thing`\n\n```xml\n<summary>\nThing.\n</summary>\n```\n", Read("document/a.cs.md"));
    }

    [Fact]
    // A top comment separated by a blank line belongs to the file; one touching the namespace belongs to the namespace.
    public void test_strip_file_comment_split()
    {
        Put("a.cs", "// The file.\n\n// The namespace.\nnamespace A\n{\n    class B { }\n}\n");
        Run();
        Assert.Equal("namespace A\n{\n    class B { }\n}\n", Read("a.cs"));
        var doc = Read("document/a.cs.md")!;
        Assert.Contains("## File\n\nThe file.\n", doc);
        Assert.Contains("## namespace `A`\n\nThe namespace.\n", doc);
    }

    [Fact]
    // Comments among top-level statements are local and stay; the file's own comment moves.
    public void test_strip_top_level_statements_kept()
    {
        Put("Program.cs", "// Entry point.\n\n// says hi\nConsole.WriteLine(\"hi\"); // trailing\n");
        Run();
        Assert.Equal("// says hi\nConsole.WriteLine(\"hi\"); // trailing\n", Read("Program.cs"));
        Assert.Contains("Entry point.", Read("document/Program.cs.md"));
    }

    [Fact]
    // Parameter comments, initializer comments and tool directives stay in the code.
    public void test_strip_parameters_initializers_directives_kept()
    {
        var text = "class A\n{\n    void F(int x /* the x */) { }\n    int y = /* why */ 3;\n    // ReSharper disable once UnusedMember.Local\n    void G() { }\n    // zazzle-csharp-minifier: unresolved method under test (no // When section)\n    void H() => G();\n}\n";
        Put("a.cs", text);
        var (_, output, _) = Run();
        Assert.Equal(text, Read("a.cs"));
        Assert.Null(Read("document/a.cs.md"));
        Assert.Equal("", output);
    }

    [Fact]
    // An inline comment between two tokens leaves a space so the tokens do not join.
    public void test_strip_inline_comment_keeps_tokens_apart()
    {
        Put("a.cs", "class A\n{\n    public/*why*/int X;\n    /* lead */ int Y;\n}\n");
        Run();
        Assert.Equal("class A\n{\n    public int X;\n    int Y;\n}\n", Read("a.cs"));
    }

    [Fact]
    // Removing a comment between blank lines, or after an opening brace, leaves no doubled or stray blank line.
    public void test_strip_blank_lines_tidied()
    {
        Put("a.cs", "class A\n{\n    // first\n\n    int x;\n\n    // between\n\n    int y;\n\n    // last\n}\n");
        Run();
        Assert.Equal("class A\n{\n    int x;\n\n    int y;\n}\n", Read("a.cs"));
    }

    [Fact]
    // Overloads, generics, operators, indexers and constructors get names that tell them apart.
    public void test_strip_member_names()
    {
        Put("a.cs", """
            class G<T>
            {
                // c
                G(int a) { }
                // m1
                void M(int a) { }
                // m2
                void M(out string s, params int[] r) { s = ""; }
                // op
                public static G<T> operator +(G<T> a, G<T> b) => a;
                // ix
                public int this[int i] => i;
                // ev
                public event System.Action? E;
            }

            """);
        Run();
        var doc = Read("document/a.cs.md")!;
        Assert.Contains("## constructor `G<T>.G(int)`", doc);
        Assert.Contains("## method `G<T>.M(int)`", doc);
        Assert.Contains("## method `G<T>.M(out string, params int[])`", doc);
        Assert.Contains("## operator `G<T>.operator +(G<T>, G<T>)`", doc);
        Assert.Contains("## indexer `G<T>.this[int]`", doc);
        Assert.Contains("## event `G<T>.E`", doc);
    }

    [Fact]
    // A second run finds nothing more and leaves the document as it was; new comments are appended to it.
    public void test_strip_second_run_appends()
    {
        Put("a.cs", "// File.\nclass A { }\n");
        Run();
        var first = Read("document/a.cs.md");
        Run();
        Assert.Equal(first, Read("document/a.cs.md"));
        Put("a.cs", "class A\n{\n    // New.\n    int x;\n}\n");
        Run();
        Assert.Equal(first + "\n## field `A.x`\n\nNew.\n", Read("document/a.cs.md"));
    }

    [Fact]
    // Generated files, build output and hidden folders are left alone.
    public void test_strip_skips_generated_and_build_output()
    {
        Put("a.g.cs", "// gen\nclass A { }\n");
        Put("b.cs", "// <auto-generated/>\nclass B { }\n");
        Put("obj/c.cs", "// built\nclass C { }\n");
        Put(".hidden/d.cs", "// hidden\nclass D { }\n");
        var (code, output, _) = Run();
        Assert.Equal(0, code);
        Assert.Contains("a.g.cs: skipped, generated code", output);
        Assert.Contains("b.cs: skipped, generated code", output);
        Assert.DoesNotContain("c.cs", output);
        Assert.False(Directory.Exists(Path.Combine(root, "document")));
    }

    [Fact]
    // A byte-order mark and CRLF line endings survive stripping.
    public void test_strip_keeps_bom_and_crlf()
    {
        var path = Path.Combine(root, "a.cs");
        File.WriteAllBytes(path, [0xEF, 0xBB, 0xBF, .. System.Text.Encoding.UTF8.GetBytes("// File.\r\nclass A\r\n{\r\n    // x\r\n    int x;\r\n}\r\n")]);
        Run();
        var bytes = File.ReadAllBytes(path);
        Assert.Equal([0xEF, 0xBB, 0xBF], bytes[..3]);
        Assert.Equal("class A\r\n{\r\n    int x;\r\n}\r\n", System.Text.Encoding.UTF8.GetString(bytes[3..]));
    }

    [Fact]
    // A file that is not UTF-8 is reported and left as it was, and the exit code says so.
    public void test_strip_not_utf8_fails()
    {
        var path = Path.Combine(root, "a.cs");
        byte[] latin1 = [.. "// caf"u8.ToArray(), 0xE9, .. "\nclass A { }\n"u8.ToArray()];
        File.WriteAllBytes(path, latin1);
        var (code, _, errors) = Run();
        Assert.Equal(1, code);
        Assert.Contains("a.cs: left unchanged: not UTF-8", errors);
        Assert.Equal(latin1, File.ReadAllBytes(path));
    }

    [Fact]
    // Plain turns each comment form into its words.
    public void test_plain_comment_forms()
    {
        Assert.Equal("hello", CommentDoc.Plain("// hello"));
        Assert.Equal("a\nb", CommentDoc.Plain("/*\n * a\n * b\n */"));
        Assert.Equal("one", CommentDoc.Plain("/* one */"));
        Assert.Equal("<summary>x</summary>", CommentDoc.Plain("/// <summary>x</summary>"));
        Assert.Equal("", CommentDoc.Plain("/**/"));
    }
}
