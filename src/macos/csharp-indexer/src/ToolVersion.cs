// Holds the tool version reported to MCP clients, the daemon handshake and the usage log, taken from the build (the release tag).
using System.Reflection;

namespace CSharpIndexer;

// The version the assembly was built with.
public static class ToolVersion
{
    public static readonly string Text =
        (typeof(ToolVersion).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion ?? "0.0.0-dev").Split('+')[0];

    // True when another version is newer than this one, comparing major.minor.patch; a development build (0.0.0-dev) counts as newest, so local builds take over.
    public static bool Newer(string other, string? mine = null)
    {
        mine ??= Text;
        static (bool dev, int[] parts) Parse(string v) =>
            (v.Contains("-dev"), v.Split('-')[0].Split('.').Select(p => int.TryParse(p, out var n) ? n : 0).Concat([0, 0, 0]).Take(3).ToArray());
        var (od, o) = Parse(other);
        var (md, m) = Parse(mine);
        if (od != md)
            return od;
        for (var i = 0; i < 3; i++)
            if (o[i] != m[i])
                return o[i] > m[i];
        return false;
    }
}
