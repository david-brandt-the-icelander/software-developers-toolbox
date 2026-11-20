// The indented identifier tree that --list-identifiers prints and --rename reads back with "=> newName" directives.
using System.Text;
using System.Text.RegularExpressions;

namespace CSharpMinifier;

// One name in the tree, its children by name, and for a parsed rename file its new name and line number.
internal sealed class IdentifierNode(string name)
{
    public string Name { get; } = name;
    public SortedDictionary<string, IdentifierNode> Children { get; } = new(StringComparer.Ordinal);
    public string? NewName { get; set; }
    public int Line { get; set; }

    // The child with that name, created when missing.
    public IdentifierNode Child(string name)
    {
        if (!Children.TryGetValue(name, out var child))
            Children[name] = child = new IdentifierNode(name);
        return child;
    }
}

// One requested rename: the old dotted path from the outermost namespace, the new last name, and its line in the file.
internal sealed record RenameRequest(IReadOnlyList<string> Path, string NewName, int Line)
{
    // The old path joined with dots.
    public string Dotted => string.Join(".", Path);
}

// Renders a tree as text and parses rename files.
internal static partial class IdentifierTree
{
    // Spaces per level in printed trees.
    private const int Indent = 4;

    // The tree as indented lines: a name with children ends with a colon.
    public static string Render(IdentifierNode root)
    {
        var sb = new StringBuilder();
        foreach (var child in root.Children.Values)
            Render(child, 0, sb);
        return sb.ToString();
    }

    // Appends one node and its children.
    private static void Render(IdentifierNode node, int depth, StringBuilder sb)
    {
        sb.Append(' ', depth * Indent).Append(node.Name).Append(node.Children.Count > 0 ? ":" : "").Append('\n');
        foreach (var child in node.Children.Values)
            Render(child, depth + 1, sb);
    }

    // The renames in a rename file, deepest first, or the errors that make it unusable.
    public static (List<RenameRequest> Renames, List<string> Errors) Parse(string text)
    {
        var renames = new List<RenameRequest>();
        var errors = new List<string>();
        var stack = new List<(int Indent, string Name)>();
        var lines = text.Replace("\r\n", "\n").Split('\n');
        for (var i = 0; i < lines.Length; i++)
        {
            var raw = lines[i];
            if (raw.Trim().Length == 0 || raw.TrimStart().StartsWith('#'))
                continue;
            var match = LineShape().Match(raw);
            if (!match.Success)
            {
                errors.Add($"line {i + 1}: expected 'name', 'name:' or 'name => newName': {raw.Trim()}");
                continue;
            }
            var indent = match.Groups["indent"].Value.Replace("\t", "    ").Length;
            while (stack.Count > 0 && stack[^1].Indent >= indent)
                stack.RemoveAt(stack.Count - 1);
            var name = match.Groups["name"].Value;
            var path = stack.Select(s => s.Name).Append(name).ToList();
            stack.Add((indent, name));
            if (match.Groups["new"].Success)
                Add(renames, errors, path, match.Groups["new"].Value, i + 1);
        }
        return (renames.OrderByDescending(r => r.Path.Count).ThenBy(r => r.Line).ToList(), errors);
    }

    // Adds a rename after checking the new name is a usable C# identifier.
    private static void Add(List<RenameRequest> renames, List<string> errors, List<string> path, string newName, int line)
    {
        if (!Microsoft.CodeAnalysis.CSharp.SyntaxFacts.IsValidIdentifier(newName)
            || Microsoft.CodeAnalysis.CSharp.SyntaxFacts.GetKeywordKind(newName) != Microsoft.CodeAnalysis.CSharp.SyntaxKind.None)
        {
            errors.Add($"line {line}: '{newName}' is not a valid C# identifier");
            return;
        }
        if (newName == path[^1])
            return;
        renames.Add(new RenameRequest(path, newName, line));
    }

    // One line of a tree: indentation, a name, an optional "=> newName" and an optional colon.
    [GeneratedRegex(@"^(?<indent>[ \t]*)(?<name>@?[\p{L}\p{Nl}_][\p{L}\p{Nl}\p{Nd}\p{Mn}\p{Mc}\p{Pc}\p{Cf}]*)\s*(=>\s*(?<new>\S+?))?\s*:?\s*$")]
    private static partial Regex LineShape();
}
