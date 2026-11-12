// Per-blob trigram Bloom filter: a file can only match a literal if all its trigrams may be present.
using System.Text;

namespace CodeIndexd.Analysis;

// Static builder and tester over ASCII-lowercased UTF-8 byte trigrams.
public static class Bloom
{
    private const int MinBits = 256;
    private const int MaxBits = 1 << 21;

    // Packs three bytes into a trigram key.
    private static int Gram(byte a, byte b, byte c) => (a << 16) | (b << 8) | c;

    // Lowercases ASCII letters only, so folding matches on both build and query side.
    private static byte Lower(byte b) => b is >= (byte)'A' and <= (byte)'Z' ? (byte)(b + 32) : b;

    // Returns two bit positions for a trigram in a filter of the given power-of-two bit count.
    private static (uint, uint) Bits(int gram, uint mask)
    {
        var h1 = (uint)gram * 2654435761u;
        h1 ^= h1 >> 15;
        var h2 = (uint)gram * 2246822519u;
        h2 ^= h2 >> 13;
        return (h1 & mask, h2 & mask);
    }

    // Builds the filter for a file's bytes, sized to its number of distinct trigrams.
    public static byte[] Build(byte[] bytes)
    {
        var grams = new HashSet<int>();
        for (var i = 0; i + 2 < bytes.Length; i++)
            grams.Add(Gram(Lower(bytes[i]), Lower(bytes[i + 1]), Lower(bytes[i + 2])));
        var bits = MinBits;
        while (bits < grams.Count * 10 && bits < MaxBits)
            bits <<= 1;
        var filter = new byte[bits / 8];
        foreach (var g in grams)
        {
            var (a, b) = Bits(g, (uint)bits - 1);
            filter[a >> 3] |= (byte)(1 << (int)(a & 7));
            filter[b >> 3] |= (byte)(1 << (int)(b & 7));
        }
        return filter;
    }

    // Returns the distinct trigrams of the given literal runs (each run is lowercased UTF-8 bytes).
    public static List<int> Grams(IEnumerable<byte[]> runs)
    {
        var set = new HashSet<int>();
        foreach (var r in runs)
            for (var i = 0; i + 2 < r.Length; i++)
                set.Add(Gram(Lower(r[i]), Lower(r[i + 1]), Lower(r[i + 2])));
        return set.Take(64).ToList();
    }

    // Reports whether every trigram may be present in the filter.
    public static bool MayContainAll(byte[] filter, List<int> grams)
    {
        if (filter.Length == 0)
            return false;
        var mask = (uint)filter.Length * 8 - 1;
        foreach (var g in grams)
        {
            var (a, b) = Bits(g, mask);
            if ((filter[a >> 3] & (1 << (int)(a & 7))) == 0 || (filter[b >> 3] & (1 << (int)(b & 7))) == 0)
                return false;
        }
        return true;
    }

    // Encodes a literal as lowercase UTF-8 bytes.
    public static byte[] Encode(string s) => Encoding.UTF8.GetBytes(s).Select(Lower).ToArray();
}
