// In-memory facts about one analysed blob, shared by every file with the same content.
namespace CodeIndexd.Index;

// Immutable blob facts; Bloom is the trigram filter used by search.
public sealed record BlobMeta(int Id, string Oid, long Size, bool Binary, string Lang, int Lines, byte[] Bloom, string Digest);
