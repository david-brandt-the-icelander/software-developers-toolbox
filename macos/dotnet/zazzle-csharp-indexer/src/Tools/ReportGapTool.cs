// Tool for the agent to say, in a sentence, what it needed and could not get; zazzle offers it as zazzle_report_gap.
using System.Text.Json.Nodes;

namespace ZazzleIndexer.Tools;

// Writes the note to the usage log, where the report command lists it for the next version.
public sealed class ReportGapTool : ITool
{
    public string Name => "report_gap";

    // Records the note.
    public string Run(JsonNode? args, CallInfo info)
    {
        info.Path = "index";
        info.Reason = "gap_report";
        info.Note = Args.Str(args, "what");
        return "recorded";
    }
}
