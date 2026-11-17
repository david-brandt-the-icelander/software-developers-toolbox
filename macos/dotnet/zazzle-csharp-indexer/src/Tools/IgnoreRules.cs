// Reads a root's `.ignore` file (ripgrep style, gitignore syntax) so overview counts what search tools search.
using System.Text.RegularExpressions;

namespace ZazzleIndexer.Tools;

// Matches relative paths against the patterns of a `.ignore` file; negated (`!`) patterns are not supported and skipped.
public sealed class IgnoreRules
{
    private readonly List<(Regex Pattern, bool DirOnly)> _rules = [];

    // Loads `.ignore` from a directory; no file means nothing is ignored.
    public static IgnoreRules Load(string dir)
    {
        var rules = new IgnoreRules();
        var file = Path.Combine(dir, ".ignore");
        if (File.Exists(file))
            foreach (var raw in File.ReadLines(file))
                rules.Add(raw.Trim());
        return rules;
    }

    // Adds one pattern line.
    public void Add(string line)
    {
        if (line.Length == 0 || line[0] == '#' || line[0] == '!')
            return;
        var dirOnly = line.EndsWith('/');
        var p = line.TrimEnd('/');
        var anchored = p.StartsWith('/') || p.Contains('/');
        p = p.TrimStart('/');
        var body = Regex.Escape(p).Replace(@"\*\*/", "(?:.*/)?").Replace(@"\*\*", ".*").Replace(@"\*", "[^/]*").Replace(@"\?", "[^/]");
        _rules.Add((new Regex(anchored ? $"^{body}$" : $"(?:^|/){body}$", RegexOptions.CultureInvariant), dirOnly));
    }

    // True when the file, or any directory above it, is ignored.
    public bool Ignores(string path)
    {
        if (_rules.Count == 0)
            return false;
        var parts = path.Split('/');
        for (var i = 1; i <= parts.Length; i++)
        {
            var prefix = string.Join('/', parts.Take(i));
            var isDir = i < parts.Length;
            if (_rules.Any(r => (isDir || !r.DirOnly) && r.Pattern.IsMatch(prefix)))
                return true;
        }
        return false;
    }
}
