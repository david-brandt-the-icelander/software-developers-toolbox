// Limits answers to files under folders or matching globs, relative to a root.
using System.Text.RegularExpressions;

namespace CSharpIndexer.Tools;

// A set of folder, file or glob scopes; empty means every file.
public sealed class PathScope(IEnumerable<string> scopes)
{
    private readonly List<Regex> _globs = scopes.Select(s => s.Trim().TrimStart('.', '/').TrimEnd('/')).Where(s => s != "").Select(ToRegex).ToList();

    // True when the scope is empty or the root-relative path is, or is under, one of its entries.
    public bool Matches(string path) => _globs.Count == 0 || _globs.Any(g => g.IsMatch(path));

    // Turns a folder, file or glob (`**` any folders, `*` and `?` within one name) into a regex that also matches what lies below it.
    private static Regex ToRegex(string glob)
    {
        var body = Regex.Escape(glob).Replace(@"\*\*/", "(.*/)?").Replace(@"\*\*", ".*").Replace(@"\*", "[^/]*").Replace(@"\?", "[^/]");
        return new Regex($"^{body}(/.*)?$", RegexOptions.IgnoreCase | RegexOptions.CultureInvariant);
    }
}
