// Tool for the agent to say, in a sentence, what it needed and could not get.
using System.Text.Json.Nodes;

namespace CodeIndexd.Tools;

// Writes the note to the usage log, where the report command lists it for the next version.
public sealed class ReportGapTool : ITool
{
    public string Name => "report_gap";
    public string Description => "Tell the tool maintainers what you wanted to know about the code but could not get from these tools, or what was slow, wrong or too verbose. One or two sentences; no code or secrets.";
    public JsonNode Schema => Args.Schema(["what"], ("what", "string", "What was missing or wrong"));

    // Records the note.
    public string Run(JsonNode? args, CallInfo info)
    {
        info.Path = "index";
        info.Reason = "gap_report";
        info.Note = Args.Str(args, "what");
        return "recorded";
    }
}
