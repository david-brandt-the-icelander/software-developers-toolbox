// Resolves "root:relative/path" references against the configured roots.
namespace ZazzleIndexer.Tools;

// The set of roots the server serves.
public sealed class RootSet(IReadOnlyList<Root> roots)
{
    public IReadOnlyList<Root> All { get; } = roots;

    // Splits "root:path" (or just "path" when there is one root) and returns the root and relative path.
    public (Root root, string rel) Split(string reference)
    {
        var colon = reference.IndexOf(':');
        if (colon > 0)
        {
            var root = All.FirstOrDefault(r => r.Name == reference[..colon]);
            if (root is not null)
                return (root, reference[(colon + 1)..]);
        }
        if (All.Count == 1)
            return (All[0], reference);
        throw new ToolException("bad_path", $"use root:path; roots are {string.Join(", ", All.Select(r => r.Name))}");
    }

    // Picks the roots named by an optional filter, or all of them.
    public IEnumerable<Root> Select(string? name)
    {
        if (name is null)
            return All;
        var r = All.Where(x => x.Name == name).ToList();
        return r.Count > 0 ? r : throw new ToolException("bad_root", $"unknown root: {name}");
    }
}
