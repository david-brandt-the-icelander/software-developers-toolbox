// Extracts literal strings every match of a regex must contain, for index prefiltering.
using System.Text;
using System.Text.RegularExpressions;

namespace ZazzleIndexer.Analysis;

// Static, deliberately conservative: when unsure it returns fewer (or no) literals.
public static class RegexLiterals
{
    // Returns the required literal runs of a pattern; plain text is one run.
    public static List<string> Required(string pattern, bool literal)
    {
        var runs = new List<string>();
        if (literal)
        {
            runs.Add(pattern);
            return Filter(runs, true);
        }
        if (Regex.IsMatch(pattern, @"\(\?[a-zA-Z]*x") || HasTopLevelAlternation(pattern))
            return [];
        var cur = new StringBuilder();
        var i = 0;
        while (i < pattern.Length)
        {
            var c = pattern[i];
            if (c == '\\' && i + 1 < pattern.Length)
                i = Escape(pattern, i, cur, runs);
            else if (c == '[')
                i = Break(SkipClass(pattern, i), cur, runs);
            else if (c == '(')
                i = Break(SkipGroup(pattern, i), cur, runs);
            else if (c is '.' or '^' or '$' or '|' or ')')
                i = Break(i + 1, cur, runs);
            else if (c is '*' or '?')
            {
                if (cur.Length > 0)
                    cur.Length--;
                i = Break(i + 1, cur, runs);
            }
            else if (c == '+')
                i = Break(i + 1, cur, runs);
            else if (c == '{')
            {
                var end = pattern.IndexOf('}', i);
                var zero = end > i && (pattern[(i + 1)..end].StartsWith('0') || pattern[(i + 1)..end].StartsWith(','));
                if (zero && cur.Length > 0)
                    cur.Length--;
                i = Break(end < 0 ? i + 1 : end + 1, cur, runs);
            }
            else
            {
                cur.Append(c);
                i++;
            }
        }
        Break(0, cur, runs);
        return Filter(runs, pattern.Contains("(?"));
    }

    // Ends the current run and returns the next index.
    private static int Break(int next, StringBuilder cur, List<string> runs)
    {
        if (cur.Length > 0)
            runs.Add(cur.ToString());
        cur.Clear();
        return next;
    }

    // Handles a backslash escape: punctuation is a literal, anything else breaks the run.
    private static int Escape(string p, int i, StringBuilder cur, List<string> runs)
    {
        var n = p[i + 1];
        if (!char.IsLetterOrDigit(n))
        {
            cur.Append(n);
            return i + 2;
        }
        Break(0, cur, runs);
        return n switch
        {
            'x' => i + 4,
            'u' => i + 6,
            'c' => i + 3,
            'p' or 'P' => p.IndexOf('}', i) is var e and >= 0 ? e + 1 : i + 2,
            'k' => p.IndexOf('>', i) is var e2 and >= 0 ? e2 + 1 : i + 2,
            _ when char.IsDigit(n) => SkipDigits(p, i + 1),
            _ => i + 2,
        };
    }

    // Skips a run of digits starting at index i.
    private static int SkipDigits(string p, int i)
    {
        while (i < p.Length && char.IsDigit(p[i]))
            i++;
        return i;
    }

    // Returns the index after a character class starting at i.
    private static int SkipClass(string p, int i)
    {
        var j = i + 1;
        if (j < p.Length && p[j] == '^')
            j++;
        if (j < p.Length && p[j] == ']')
            j++;
        for (; j < p.Length; j++)
        {
            if (p[j] == '\\')
                j++;
            else if (p[j] == ']')
                return j + 1;
        }
        return p.Length;
    }

    // Returns the index after the group starting at i.
    private static int SkipGroup(string p, int i)
    {
        var depth = 0;
        for (var j = i; j < p.Length; j++)
        {
            if (p[j] == '\\')
                j++;
            else if (p[j] == '[')
                j = SkipClass(p, j) - 1;
            else if (p[j] == '(')
                depth++;
            else if (p[j] == ')' && --depth == 0)
                return j + 1;
        }
        return p.Length;
    }

    // Reports whether a "|" occurs outside groups and classes.
    private static bool HasTopLevelAlternation(string p)
    {
        for (var i = 0; i < p.Length; i++)
        {
            if (p[i] == '\\')
                i++;
            else if (p[i] == '[')
                i = SkipClass(p, i) - 1;
            else if (p[i] == '(')
                i = SkipGroup(p, i) - 1;
            else if (p[i] == '|')
                return true;
        }
        return false;
    }

    // Drops runs shorter than three characters and cuts non-ASCII characters when case may be ignored.
    private static List<string> Filter(List<string> runs, bool maybeIgnoreCase)
    {
        var result = new List<string>();
        foreach (var r in runs)
        {
            if (!maybeIgnoreCase)
            {
                result.Add(r);
                continue;
            }
            var sb = new StringBuilder();
            foreach (var ch in r)
            {
                if (ch < 128)
                    sb.Append(ch);
                else
                {
                    result.Add(sb.ToString());
                    sb.Clear();
                }
            }
            result.Add(sb.ToString());
        }
        return result.Where(r => r.Length >= 3).ToList();
    }
}
