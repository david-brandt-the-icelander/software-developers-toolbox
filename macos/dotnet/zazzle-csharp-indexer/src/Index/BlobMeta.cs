// In-memory facts about one analysed blob, shared by every file with the same content.
namespace ZazzleIndexer.Index;

// Immutable blob facts kept in memory for every analysed blob.
public sealed record BlobMeta(int Id, string Oid, long Size, bool Binary, string Lang, int Lines, string Digest);
