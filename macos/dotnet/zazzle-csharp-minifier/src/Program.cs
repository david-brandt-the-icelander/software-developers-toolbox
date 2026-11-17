// Entry point: reads the command-line switches and runs the requested commands.
using ZazzleMinifier;

const string Usage = """
    usage: zazzle-csharp-minifier <commands> [--check] [--root <dir>] [<file-or-folder>...]
           zazzle-csharp-minifier --list-identifiers | --rename <file> [--root <dir>]
    commands (run in this order):
      --strip-comments  move comments on files, types and members into <root>/document/<path>.md
      --split-tests     split test files into <ClassUnderTest>/<MethodUnderTest>.cs, one file per method under test
      --split-headers   split each .cs file into declarations (stays) and implementation (src/<name>.cs beside it)
      --restructure     turn each project of the solution into <project>/incl, src, doc and test (implies the three above)
    --list-identifiers  print the namespaces, types and members as an indented tree
    --rename <file>     apply "name => newName" lines in such a tree, deepest first, moving files when namespaces or types are renamed
    --check    change nothing; list what the commands would change and exit 1 if anything would
    --root     the tree to work on (default: current directory); paths must be inside it
    --version  print the version
    """;

var parsed = Cli.Parse(args);
if (parsed.Error != null)
{
    Console.Error.WriteLine(parsed.Error);
    Console.Error.WriteLine(Usage);
    return 2;
}
if (parsed.Help)
{
    Console.WriteLine(Usage);
    return 0;
}
if (parsed.Version)
{
    Console.WriteLine("zazzle-csharp-minifier " + ToolVersion.Text);
    return 0;
}
return Runner.Run(parsed, Console.Out, Console.Error);
