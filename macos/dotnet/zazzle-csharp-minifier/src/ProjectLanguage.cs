// Works out which C# version a file is compiled with, from its nearest .csproj and the Directory.Build.props above it.
using System.Text.RegularExpressions;
using Microsoft.CodeAnalysis.CSharp;

namespace ZazzleMinifier;

// Reads LangVersion or TargetFramework the way MSBuild would for the project that holds a file.
internal static partial class ProjectLanguage
{
    // The file's C# version, or null with the reason when there is no project or the version cannot be told.
    public static (LanguageVersion? Version, string? Why) For(string root, string file)
    {
        var (lang, framework, found) = Properties(root, Path.GetDirectoryName(file)!);
        if (!found)
            return (null, "no .csproj found");
        if (lang != null && lang != "default" && LanguageVersionFacts.TryParse(lang, out var parsed))
            return (parsed.MapSpecifiedToEffectiveVersion(), null);
        if (framework == null)
            return (null, "the project names no TargetFramework or LangVersion");
        return (FromFramework(framework), null);
    }

    // The nearest project's LangVersion and TargetFramework, the project's own values first, then the nearest Directory.Build.props.
    private static (string? Lang, string? Framework, bool Found) Properties(string root, string start)
    {
        string? lang = null, framework = null;
        var found = false;
        for (var dir = start; dir != null; dir = Path.GetDirectoryName(dir))
        {
            var project = found ? null : Directory.GetFiles(dir, "*.csproj").Order(StringComparer.Ordinal).FirstOrDefault();
            if (project != null)
            {
                found = true;
                (lang, framework) = Read(project, lang, framework);
            }
            var props = Path.Combine(dir, "Directory.Build.props");
            if (found && File.Exists(props))
                (lang, framework) = Read(props, lang, framework);
            if (Path.GetRelativePath(root, dir) == "." || (found && File.Exists(props)))
                break;
        }
        return (lang, framework, found);
    }

    // Fills the values still missing from one MSBuild file, ignoring ones built from $(properties).
    private static (string? Lang, string? Framework) Read(string path, string? lang, string? framework)
    {
        var text = File.ReadAllText(path);
        lang ??= Property(text, "LangVersion");
        framework ??= Property(text, "TargetFramework") ?? Property(text, "TargetFrameworks")?.Split(';')[0].Trim();
        return (lang, framework);
    }

    // The value of the first <name> element, or null when missing or computed.
    private static string? Property(string text, string name)
    {
        var match = Regex.Match(text, $"<{name}(\\s[^>]*)?>([^<]*)</{name}>");
        var value = match.Success ? match.Groups[2].Value.Trim() : null;
        return string.IsNullOrEmpty(value) || value.Contains('$') ? null : value;
    }

    // The default C# version of a target framework moniker such as net8.0 or netstandard2.0.
    private static LanguageVersion FromFramework(string framework)
    {
        var net = NetMajor().Match(framework);
        if (net.Success && int.TryParse(net.Groups[1].Value, out var major) && major >= 5)
            return major switch
            {
                5 => LanguageVersion.CSharp9,
                6 => LanguageVersion.CSharp10,
                7 => LanguageVersion.CSharp11,
                8 => LanguageVersion.CSharp12,
                9 => LanguageVersion.CSharp13,
                _ => LanguageVersion.CSharp14,
            };
        if (framework.StartsWith("netcoreapp3", StringComparison.Ordinal) || framework == "netstandard2.1")
            return LanguageVersion.CSharp8;
        return LanguageVersion.CSharp7_3;
    }

    // Matches net5.0 and later, including platform suffixes such as net8.0-windows.
    [GeneratedRegex(@"^net(\d+)\.\d+")]
    private static partial Regex NetMajor();
}
