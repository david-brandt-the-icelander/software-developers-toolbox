// The --restructure command: turns each project of the solution into <project>/incl, src, doc and test.
namespace ZazzleMinifier;

// Plans the move of every project into the restructured layout.
internal static class Restructure
{
    // The edits that restructure the projects.
    public static Plan Make(string root, IReadOnlyList<string> files) => new();
}
