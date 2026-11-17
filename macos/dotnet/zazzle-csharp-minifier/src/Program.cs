// Entry point: reads the command-line switches and runs the requested commands.
using ZazzleMinifier;

const string Usage = """
    usage: zazzle-csharp-minifier <commands> [--check] [--root <dir>] [<file-or-folder>...]
           zazzle-csharp-minifier --install <commands> [--root <dir>]
    commands (run in this order):
      --strip-comments  move comments on files, types and members into <root>/document/<path>.md
      --split-tests     split test files into <ClassUnderTest>/<MethodUnderTest>.cs, one file per method under test
      --split-headers   split each .cs file into declarations (stays) and implementation (src/<name>.cs beside it)
    --check    change nothing; list what the commands would change and exit 1 if anything would
    --install  add the skill, an AGENTS.md entry and a pre-commit --check with these commands to the repository at --root
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
if (parsed.Install)
    return Install.Run(parsed.Root, parsed.CommandSwitches(), Console.Out, Console.Error);
return Runner.Run(parsed, Console.Out, Console.Error);
