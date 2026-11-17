// The --split-tests command: moves each test into <ClassUnderTest>/<MethodUnderTest>.cs.
namespace ZazzleMinifier;

// Plans the test-file split.
internal static class SplitTests
{
    // The edits that split the test files.
    public static Plan Make(string root, IReadOnlyList<string> files) => new();
}
