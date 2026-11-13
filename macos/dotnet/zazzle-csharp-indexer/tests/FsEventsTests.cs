// macOS-only test of the FSEvents change source; it returns immediately on other systems.
using ZazzleIndexer.Index;
using Xunit;

namespace ZazzleIndexer.Tests;

// Exercises FSEvents delivery and event ids.
public class FsEventsTests
{
    [Fact]
    // A file created under a watched directory is reported and the event id advances.
    public void test_fsevents_reports_new_file()
    {
        if (!OperatingSystem.IsMacOS())
            return;
        var dir = Native.RealPath(Directory.CreateTempSubdirectory("fsev").FullName);
        using var src = new FsEventsSource();
        var seen = new List<string>();
        src.Changed += paths => { lock (seen) seen.AddRange(paths); };
        src.Start(dir, null);
        var start = src.LastEventId;
        File.WriteAllText(Path.Combine(dir, "x.txt"), "x");
        Assert.True(SpinWait.SpinUntil(() => { lock (seen) return seen.Any(p => p.EndsWith("x.txt")); }, 10000));
        Assert.True(src.LastEventId > start);
        Directory.Delete(dir, true);
    }
}
