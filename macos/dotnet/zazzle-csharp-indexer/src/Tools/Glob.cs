// Converts simple glob patterns to regular expressions.
using System.Text;
using System.Text.RegularExpressions;

namespace ZazzleIndexer.Tools;

// Static glob helper: "*" stays within a directory, "**" crosses directories, "?" is one character.
public static class Glob
{
    // Builds a regex matching a relative path; a glob without "/" matches the file name at any depth.
    public static Regex ToRegex(string glob)
    {
        var sb = new StringBuilder(glob.Contains('/') ? "^" : "(^|/)");
        for (var i = 0; i < glob.Length; i++)
        {
            var c = glob[i];
            if (c == '*' && i + 1 < glob.Length && glob[i + 1] == '*')
            {
                sb.Append(".*");
                i++;
                if (i + 1 < glob.Length && glob[i + 1] == '/')
                    i++;
            }
            else if (c == '*')
                sb.Append("[^/]*");
            else if (c == '?')
                sb.Append("[^/]");
            else
                sb.Append(Regex.Escape(c.ToString()));
        }
        return new Regex(sb.Append('$').ToString(), RegexOptions.CultureInvariant);
    }
}
