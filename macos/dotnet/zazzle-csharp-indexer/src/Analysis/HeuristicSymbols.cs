// Regex-based symbol extraction for non-C# languages; results are marked heuristic by the tools.
using System.Text.RegularExpressions;

namespace ZazzleIndexer.Analysis;

// Static ctags-style extractor.
public static class HeuristicSymbols
{
    private static readonly Dictionary<string, (string kind, Regex re)[]> Rules = new()
    {
        ["rust"] = [("type", new(@"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|type)\s+(\w+)")), ("fn", new(@"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)")), ("mod", new(@"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:mod|const|static)\s+(\w+)")), ("impl", new(@"^\s*impl(?:<[^>]*>)?\s+(?:[\w:<>]+\s+for\s+)?(\w+)"))],
        ["python"] = [("def", new(@"^\s*(?:async\s+)?def\s+(\w+)")), ("class", new(@"^\s*class\s+(\w+)"))],
        ["typescript"] = [("function", new(@"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\*?\s+(\w+)")), ("type", new(@"^\s*(?:export\s+)?(?:abstract\s+)?(?:class|interface|enum|type)\s+(\w+)")), ("const", new(@"^\s*(?:export\s+)?const\s+(\w+)\s*=\s*(?:async\s*)?(?:\(|function)"))],
        ["go"] = [("func", new(@"^func\s+(?:\([^)]*\)\s*)?(\w+)")), ("type", new(@"^type\s+(\w+)\s+(?:struct|interface)"))],
        ["java"] = [("type", new(@"^\s*(?:public\s+|private\s+|protected\s+|abstract\s+|final\s+|static\s+)*(?:class|interface|enum|record)\s+(\w+)"))],
        ["gdscript"] = [("func", new(@"^\s*(?:static\s+)?func\s+(\w+)")), ("class", new(@"^\s*(?:class_name|class)\s+(\w+)"))],
        ["shell"] = [("function", new(@"^\s*(?:function\s+)?(\w+)\s*\(\)\s*\{?")), ("function", new(@"^\s*function\s+(\w+)"))],
        ["markdown"] = [("heading", new(@"^#{1,6}\s+(.+?)\s*#*\s*$"))],
        ["swift"] = [("func", new(@"^\s*(?:public\s+|private\s+|internal\s+|static\s+)*func\s+(\w+)")), ("type", new(@"^\s*(?:public\s+|final\s+)*(?:class|struct|enum|protocol)\s+(\w+)"))],
        ["c"] = [("func", new(@"^[A-Za-z_][\w\s\*]*?\b(\w+)\s*\([^;]*$")), ("type", new(@"^\s*(?:typedef\s+)?(?:struct|enum|union)\s+(\w+)"))],
    };

    // Extracts symbols with the language's rules; "javascript" and "cpp" reuse the TypeScript and C rules.
    public static List<Sym> Extract(string source, string lang)
    {
        var rules = Rules.GetValueOrDefault(lang switch { "javascript" => "typescript", "cpp" => "c", _ => lang });
        var lines = source.Split('\n');
        var syms = new List<Sym>();
        if (rules is not null)
            for (var i = 0; i < lines.Length; i++)
                foreach (var (kind, re) in rules)
                {
                    var m = re.Match(lines[i]);
                    if (!m.Success)
                        continue;
                    var sig = lines[i].Trim();
                    syms.Add(new Sym(m.Groups[1].Value, kind, "", i + 1, i + 1, sig.Length > 140 ? sig[..140] : sig));
                    break;
                }
        return syms;
    }
}
