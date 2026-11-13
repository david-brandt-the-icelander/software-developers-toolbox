// Tests for the blob store, stat-key tracking, sharing across copies and change watching.
using ZazzleIndexer.Analysis;
using Xunit;

namespace ZazzleIndexer.Tests;

// Exercises the root index against temporary repositories.
public class IndexTests
{
    private static readonly Dictionary<string, string> Files = new() { ["a.txt"] = "hello\nworld\n", ["d/b.cs"] = "class B {}\n" };

    [Fact]
    // Our blob id equals what git hash-object computes for the same bytes.
    public void test_oid_matches_git_hash_object()
    {
        using var repo = new TempRepo(Files);
        var expected = Proc.Run("git", ["-C", repo.Dir, "hash-object", "a.txt"], 20).Stdout.Trim();
        Assert.Equal(expected, BlobAnalyzer.GitOid(File.ReadAllBytes(Path.Combine(repo.Dir, "a.txt"))));
    }

    [Fact]
    // The initial reconcile indexes every non-ignored file and counts lines.
    public void test_reconcile_indexes_files()
    {
        using var repo = new TempRepo(Files);
        var idx = repo.Root().Index;
        Assert.Equal(["a.txt", "d/b.cs"], idx.Files().Select(f => f.Path));
        Assert.Equal(2, repo.Engine.Blob(idx.Get("a.txt")!.BlobId)!.Lines);
        Assert.Equal("csharp", repo.Engine.Blob(idx.Get("d/b.cs")!.BlobId)!.Lang);
    }

    [Fact]
    // Two copies of a repo with the same content share the same blob ids.
    public void test_copies_share_blobs()
    {
        using var one = new TempRepo(Files);
        using var two = new TempRepo(Files, one.Engine);
        var a = one.Root().Index.Get("a.txt")!;
        var b = two.Root().Index.Get("a.txt")!;
        Assert.Equal(a.BlobId, b.BlobId);
    }

    [Fact]
    // Changing, adding and deleting files updates the index and the change log after a sync.
    public void test_sync_tracks_changes()
    {
        using var repo = new TempRepo(Files);
        var idx = repo.Root().Index;
        var before = idx.Gen;
        var oldBlob = idx.Get("a.txt")!.BlobId;
        File.WriteAllText(Path.Combine(repo.Dir, "a.txt"), "changed\n");
        File.WriteAllText(Path.Combine(repo.Dir, "new.txt"), "new\n");
        File.Delete(Path.Combine(repo.Dir, "d/b.cs"));
        idx.Sync();
        Assert.NotEqual(oldBlob, idx.Get("a.txt")!.BlobId);
        Assert.Null(idx.Get("d/b.cs"));
        var changes = idx.ChangesSince(before, out var expired).ToDictionary(c => c.path, c => c.kind);
        Assert.False(expired);
        Assert.Equal('M', changes["a.txt"]);
        Assert.Equal('A', changes["new.txt"]);
        Assert.Equal('D', changes["d/b.cs"]);
    }

    [Fact]
    // Stored file records survive a restart and unchanged files are not re-analysed.
    public void test_state_persists_across_restart()
    {
        var state = Directory.CreateTempSubdirectory("zazzle-csharp-indexer-state").FullName;
        using var repo = new TempRepo(Files, Engine.Open(state, false));
        var blob = repo.Root().Index.Get("a.txt")!.BlobId;
        repo.Engine.Dispose();
        using var again = Engine.Open(state, false);
        var idx = Root.Parse("r=" + repo.Dir, again).Index;
        Assert.Equal(blob, idx.Get("a.txt")!.BlobId);
        Assert.Equal(blob, again.Blob(blob)!.Id);
    }

    [Fact]
    // The live watcher picks up a new file and an edit without an explicit sync.
    public void test_watcher_sees_new_file()
    {
        using var repo = new TempRepo(Files, watch: true);
        var idx = repo.Root().Index;
        File.WriteAllText(Path.Combine(repo.Dir, "live.txt"), "x\n");
        Assert.True(SpinWait.SpinUntil(() => idx.Get("live.txt") is not null, 10000));
        File.WriteAllText(Path.Combine(repo.Dir, "live.txt"), "xyz changed\n");
        var blob = idx.Get("live.txt")!.BlobId;
        Assert.True(SpinWait.SpinUntil(() => idx.Get("live.txt")!.BlobId != blob, 10000));
    }

    [Fact]
    // Ignored files never enter the index, even when the watcher reports them.
    public void test_watcher_ignores_gitignored()
    {
        using var repo = new TempRepo(new Dictionary<string, string>(Files) { [".gitignore"] = "out/\n" }, watch: true);
        var idx = repo.Root().Index;
        Directory.CreateDirectory(Path.Combine(repo.Dir, "out"));
        File.WriteAllText(Path.Combine(repo.Dir, "out/junk.txt"), "x\n");
        File.WriteAllText(Path.Combine(repo.Dir, "ok.txt"), "x\n");
        Assert.True(SpinWait.SpinUntil(() => idx.Get("ok.txt") is not null, 10000));
        Assert.Null(idx.Get("out/junk.txt"));
    }
}
