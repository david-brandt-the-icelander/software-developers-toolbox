// The --rename command: applies "name => newName" lines of an identifier tree, deepest first.
namespace ZazzleMinifier;

// Plans the renames and the file moves they bring.
internal static class Rename
{
    // The edits that perform the renames.
    public static Plan Make(string root, string renameFile) => new();
}
