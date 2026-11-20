// Entry point: reads the command-line switches and runs the requested commands.
using CSharpMinifier;

const string Usage = """
    usage: csharp-minifier <commands> [--check] [--root <dir>] [<file-or-folder>...]
           csharp-minifier --list-identifiers | --rename <file> [--root <dir>]
           csharp-minifier --benchmark <prompt-file-or-folder> --model <model> --effort <level> [--root <dir>] [<commands>]
    commands (run in this order):
      --allow-reads     allow agents the read-only commands (ls, cat, head, tail, wc, grep, rg) in the repository's .claude/settings.json
      --split-tests     split test files into <ClassUnderTest>/<MethodUnderTest>.cs, one file per method under test
    --list-identifiers  print the namespaces, types and members as an indented tree
    --rename <file>     apply "name => newName" lines in such a tree, deepest first, moving files when namespaces or types are renamed
    --benchmark <p>     clone the root to ../benchmark-<name> (cp -Rc), apply the commands to the clone, then run each prompt
                        file three times in fresh `claude -p` sessions; results in ~/.zazzle/agent-token-benchmarks
    --model, --effort   the model and effort (low, medium, high, xhigh, max) for the benchmark sessions
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
    Console.WriteLine("csharp-minifier " + ToolVersion.Text);
    return 0;
}
return Runner.Run(parsed, Console.Out, Console.Error);
