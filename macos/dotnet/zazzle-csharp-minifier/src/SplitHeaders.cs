// The --split-headers command: splits each .cs file into declarations and a src/ implementation.
namespace ZazzleMinifier;

// Plans the header/implementation split.
internal static class SplitHeaders
{
    // The edits that split the files.
    public static Plan Make(string root, IReadOnlyList<string> files) => new();
}
