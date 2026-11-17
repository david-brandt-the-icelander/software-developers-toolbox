// Tests for command-line parsing.
using Xunit;

namespace ZazzleMinifier.Tests;

// Checks the switches and the path rules.
public class CliTests
{
    [Fact]
    // No command switch is an error.
    public void test_parse_nothing_to_do()
    {
        Assert.StartsWith("nothing to do", Cli.Parse([]).Error);
    }

    [Fact]
    // An unknown switch, or --root without a value, is an error.
    public void test_parse_bad_switches()
    {
        Assert.StartsWith("unknown or incomplete switch: --nope", Cli.Parse(["--strip-comments", "--nope"]).Error);
        Assert.StartsWith("unknown or incomplete switch: --root", Cli.Parse(["--strip-comments", "--root"]).Error);
    }

    [Fact]
    // --root sets the root, and with no paths the whole root is processed.
    public void test_parse_root_default_paths()
    {
        var dir = Directory.CreateTempSubdirectory("zazzle-csharp-minifier").FullName;
        var parsed = Cli.Parse(["--strip-comments", "--root", dir]);
        Assert.Null(parsed.Error);
        Assert.Equal(dir, parsed.Root);
        Assert.Equal([dir], parsed.Paths);
        Directory.Delete(dir);
    }

    [Fact]
    // A path outside the root, or one that does not exist, is an error.
    public void test_parse_path_outside_root()
    {
        var dir = Directory.CreateTempSubdirectory("zazzle-csharp-minifier").FullName;
        var sub = Directory.CreateDirectory(Path.Combine(dir, "sub")).FullName;
        Assert.StartsWith("path is missing or outside the root", Cli.Parse(["--strip-comments", "--root", sub, dir]).Error);
        Assert.StartsWith("path is missing or outside the root", Cli.Parse(["--strip-comments", "--root", dir, Path.Combine(dir, "nope.cs")]).Error);
        Assert.Null(Cli.Parse(["--strip-comments", "--root", dir, sub]).Error);
        Directory.Delete(dir, true);
    }

    [Fact]
    // --help and --version need no command.
    public void test_parse_help_version()
    {
        Assert.True(Cli.Parse(["--help"]).Help);
        Assert.True(Cli.Parse(["--version"]).Version);
    }
}
