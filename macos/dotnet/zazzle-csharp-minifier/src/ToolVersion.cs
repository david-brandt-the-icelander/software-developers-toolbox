// Holds the tool version printed by --version, taken from the build.
using System.Reflection;

namespace ZazzleMinifier;

// The version the assembly was built with.
internal static class ToolVersion
{
    public static readonly string Text =
        (typeof(ToolVersion).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion ?? "0.0.0-dev").Split('+')[0];
}
