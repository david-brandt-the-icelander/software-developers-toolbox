// Holds the tool version reported to MCP clients, the daemon handshake and the usage log, taken from the build (the release tag).
using System.Reflection;

namespace ZazzleIndexer;

// The version the assembly was built with.
public static class ToolVersion
{
    public static readonly string Text =
        (typeof(ToolVersion).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion ?? "0.0.0-dev").Split('+')[0];
}
