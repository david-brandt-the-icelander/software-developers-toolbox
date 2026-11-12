// Builds token-budgeted text output for tools.
using System.Text;

namespace CodeIndexd.Tools;

// Accumulates lines until a byte budget derived from max_tokens is reached.
public sealed class Output(int maxTokens)
{
    private readonly StringBuilder _sb = new();
    private readonly int _budget = Math.Max(256, maxTokens * 4);

    public bool Full { get; private set; }

    // Adds a line unless it would exceed the budget; returns false once full.
    public bool Add(string line)
    {
        if (Full)
            return false;
        if (_sb.Length + line.Length + 1 > _budget)
        {
            Full = true;
            return false;
        }
        _sb.Append(line).Append('\n');
        return true;
    }

    // Returns the text built so far plus a footer line.
    public string Finish(string footer) => _sb.Append(footer).ToString();
}
