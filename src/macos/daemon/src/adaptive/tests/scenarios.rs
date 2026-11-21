// Acceptance scenarios for adaptive investigation from one unseeded call: a multi-project C# repository under a workspace path that names MCP, with an MCP decoy, Django-facing controllers, an interface-backed service, an idempotency filter, a cache and separate test projects, driven by scripted model replies through the production path.
use super::*;

// The workspace and repository the scenarios run in; the workspace path names MCP, like the benchmark's.
const REPO: &str = "MCP Proxy Benchmark/fenix";

// The fixture's files: (path, text).
const FILES: &[(&str, &str)] = &[
    (
        "Mcp/AGENTS.md",
        "# MCP proxy tools\n\nThis subproject exposes the Fenix log tools over MCP.\nNo HTTP endpoint in this repository uses idempotency keys.\n",
    ),
    (
        "Mcp/Tools/FenixLogsReadMachineLogFileTool.cs",
        "namespace Fenix.Mcp.Tools;\n\n// The MCP proxy tool that reads a machine's log file for the benchmark.\npublic class FenixLogsReadMachineLogFileTool\n{\n    [HttpPost(\"mcp/proxy/benchmark/logs\")]\n    public string ReadMachineLogFile(string machine, string file)\n    {\n        return Fenix.Logs.Read(machine, file);\n    }\n}\n",
    ),
    (
        "Api/Controllers/DjangoCarWashesFacadeV1Controller.cs",
        "using Fenix.Api.Filters;\nusing Fenix.Core.Services;\n\nnamespace Fenix.Api.Controllers;\n\n[ApiController]\n[Route(\"api/v1/DjangoCarWashesFacade\")]\npublic class DjangoCarWashesFacadeV1Controller : ControllerBase\n{\n    private readonly IDjangoFacadeService _djangoFacadeService;\n\n    public DjangoCarWashesFacadeV1Controller(IDjangoFacadeService djangoFacadeService)\n    {\n        _djangoFacadeService = djangoFacadeService;\n    }\n\n    [HttpPost(\"subscriptions/{subscriptionId}/wash\")]\n    [ServiceFilter(typeof(IdempotencyKeyFilter))]\n    public async Task<IActionResult> StartSubscriptionWash(string subscriptionId, [FromHeader(Name = \"OperationId\")] string operationId)\n    {\n        var commandContext = new WashCommandContext(subscriptionId, operationId);\n        var result = await _djangoFacadeService.StartSubscriptionWash(commandContext);\n        return Ok(result);\n    }\n}\n",
    ),
    (
        "Api/Filters/IdempotencyKeyFilter.cs",
        "namespace Fenix.Api.Filters;\n\npublic class IdempotencyKeyFilter : IAsyncActionFilter\n{\n    private readonly IIdempotencyStore _store;\n\n    public IdempotencyKeyFilter(IIdempotencyStore store)\n    {\n        _store = store;\n    }\n\n    public async Task OnActionExecutionAsync(ActionExecutingContext context, ActionExecutionDelegate next)\n    {\n        var headers = context.HttpContext.Request.Headers;\n        string key = headers[\"Idempotency-Key\"];\n        if (string.IsNullOrWhiteSpace(key))\n        {\n            key = headers[\"X-Operation-Id\"];\n        }\n        if (string.IsNullOrWhiteSpace(key))\n        {\n            await next();\n            return;\n        }\n        if (_store.TryGetReplay(key, out var replay))\n        {\n            context.Result = replay;\n            return;\n        }\n        var executed = await next();\n        _store.Save(key, executed.Result);\n    }\n}\n",
    ),
    (
        "Core/Services/IDjangoFacadeService.cs",
        "namespace Fenix.Core.Services;\n\npublic interface IDjangoFacadeService\n{\n    Task<WashResult> StartSubscriptionWash(WashCommandContext commandContext);\n}\n",
    ),
    (
        "Core/Services/DjangoFacadeService.cs",
        "namespace Fenix.Core.Services;\n\npublic class DjangoFacadeService : IDjangoFacadeService\n{\n    private readonly IWashRepository _washRepository;\n    private readonly IWashWorkflow _washWorkflow;\n    private readonly ILogger _logger;\n\n    public async Task<WashResult> StartSubscriptionWash(WashCommandContext commandContext)\n    {\n        _logger.LogInformation(\"starting a subscription wash\");\n        var wash = await _washRepository.InsertWash(commandContext.SubscriptionId, commandContext.OperationId);\n        await _washWorkflow.EnqueueWash(wash.Id);\n        return new WashResult(wash.Id);\n    }\n}\n",
    ),
    (
        "Core/Persistence/WashRepository.cs",
        "namespace Fenix.Core.Persistence;\n\npublic interface IWashRepository\n{\n    Task<Wash> InsertWash(string subscriptionId, string operationId);\n}\n\npublic class WashRepository : IWashRepository\n{\n    private readonly WashDbContext _db;\n\n    public async Task<Wash> InsertWash(string subscriptionId, string operationId)\n    {\n        var wash = new Wash(subscriptionId, operationId);\n        _db.Washes.Add(wash);\n        await _db.SaveChangesAsync();\n        return wash;\n    }\n}\n",
    ),
    (
        "Core/Workflow/WashWorkflow.cs",
        "namespace Fenix.Core.Workflow;\n\npublic interface IWashWorkflow\n{\n    Task EnqueueWash(Guid washId);\n}\n\npublic class WashWorkflow : IWashWorkflow\n{\n    public Task EnqueueWash(Guid washId)\n    {\n        return Queue.Publish(new StartWash(washId));\n    }\n}\n",
    ),
    (
        "Core/Caching/ITerminalPollResponseCache.cs",
        "namespace Fenix.Core.Caching;\n\npublic interface ITerminalPollResponseCache\n{\n    void Remember(string terminalId, PollResponse response, DateTimeOffset at);\n    bool Recall(string terminalId, DateTimeOffset now, out PollResponse response);\n}\n",
    ),
    (
        "Core/Caching/TerminalPollResponseCache.cs",
        "namespace Fenix.Core.Caching;\n\npublic class TerminalPollResponseCache : ITerminalPollResponseCache\n{\n    private readonly Dictionary<string, Entry> _entries = new();\n\n    public void Remember(string terminalId, PollResponse response, DateTimeOffset at)\n    {\n        _entries[terminalId] = new Entry(response, at);\n    }\n\n    public bool Recall(string terminalId, DateTimeOffset now, out PollResponse response)\n    {\n        response = null;\n        if (!_entries.TryGetValue(terminalId, out var entry))\n        {\n            return false;\n        }\n        if (entry.At > now)\n        {\n            return false;\n        }\n        response = entry.Response;\n        return true;\n    }\n}\n",
    ),
    (
        "Core/Services/CarWashesService.TerminalPoll.cs",
        "namespace Fenix.Core.Services;\n\npublic partial class CarWashesService\n{\n    private readonly ITerminalPollResponseCache _terminalPollResponseCache;\n    private readonly IPollMetrics _metrics;\n\n    public PollResponse PollTerminal(string terminalId, DateTimeOffset now)\n    {\n        if (_terminalPollResponseCache.Recall(terminalId, now, out var cached))\n        {\n            _metrics.IncrementServedFromMemory();\n            return cached;\n        }\n        var fresh = FetchFromTerminal(terminalId);\n        _terminalPollResponseCache.Remember(terminalId, fresh, now);\n        return fresh;\n    }\n}\n",
    ),
    (
        "Tests/Core.Tests/TerminalPollResponseCacheTests.cs",
        "namespace Fenix.Core.Tests;\n\npublic class TerminalPollResponseCacheTests\n{\n    [Fact]\n    public void Recall_RefusesAnEntryStampedInTheFuture()\n    {\n        var cache = new TerminalPollResponseCache();\n        cache.Remember(\"t1\", Response, Now.AddMinutes(1));\n        Assert.False(cache.Recall(\"t1\", Now, out _));\n    }\n}\n",
    ),
    (
        "Tests/Api.Tests/TerminalPollFromMemoryTests.cs",
        "namespace Fenix.Api.Tests;\n\npublic class TerminalPollFromMemoryTests\n{\n    [Fact]\n    public void PollTerminal_ServedFromMemory_CountsTheMetric()\n    {\n        var service = Service(cache: Remembered(\"t1\"), metrics: Metrics);\n        service.PollTerminal(\"t1\", Now);\n        Assert.Equal(1, Metrics.ServedFromMemory);\n    }\n}\n",
    ),
    (
        "Tests/Api.Tests/IdempotencyKeyFilterTests.cs",
        "namespace Fenix.Api.Tests;\n\npublic class IdempotencyKeyFilterTests\n{\n    [Fact]\n    public async Task BlankKey_RunsTheActionWithoutReplay()\n    {\n        var ran = await Run(new IdempotencyKeyFilter(Store), headers: Blank);\n        Assert.True(ran);\n    }\n}\n",
    ),
];

// Writes the fixture, without the files named in `leave_out`; returns the folder and the repository's canonical root.
fn fixture(leave_out: &[&str]) -> (TempDir, PathBuf) {
    let t = TempDir::new();
    std::fs::create_dir_all(t.path().join(REPO).join(".git")).expect("git");
    for (path, text) in FILES.iter().filter(|(p, _)| !leave_out.contains(p)) {
        t.write(&format!("{REPO}/{path}"), text.as_bytes());
    }
    let root = t.path().join(REPO).canonicalize().expect("root");
    (t, root)
}

// The 1-based line of a fixture file that holds `needle` first.
fn at(path: &str, needle: &str) -> usize {
    let text = FILES.iter().find(|(p, _)| *p == path).map(|(_, t)| *t).expect("a fixture file");
    text.lines().position(|l| l.contains(needle)).expect("the needle") + 1
}

// The operational preamble every scenario's prompt starts with: where it runs and how, naming MCP.
const PREAMBLE: &str =
    "You are running the MCP Proxy Benchmark from /Users/kisi/MCP Proxy Benchmark/fenix. Use zazzle only and do not edit files.";

// Runs one unseeded adaptive call on the Low/Medium ladder with JSON output, returning the text report and the JSON report from two identical scripts.
fn run(root: &Path, prompt: &str, script: &[(&str, Result<&str, &str>)]) -> (String, Value, Fake) {
    let argv = |json: bool| {
        let mut a = vec![prompt, "--adaptive", "--agents", "qwen-a@low,qwen-a@medium", "--steps", "4"];
        if json {
            a.push("--json");
        }
        a
    };
    let text = go(root, &Fake::new(script), &argv(false)).expect("text run");
    let f = Fake::new(script);
    let v: Value = serde_json::from_str(&go(root, &f, &argv(true)).expect("json run")).expect("json");
    (text, v, f)
}

// Checks the reproduced cause of the first failure: ranked by the whole prompt, the MCP decoy is the first candidate; ranked by the task's subject, it is not among them.
#[test]
fn test_subject_not_prompt_ranks_discovery() {
    let (_t, root) = fixture(&[]);
    let prompt = format!("{PREAMBLE} (1) Which Django-facing HTTP endpoints implemented in C# require idempotency? (2) How does the idempotency filter treat a blank key, and which headers does it read? Answer in a table.");
    let e = crate::engine::Engine::new();
    let scope = [root.clone()];
    let rank = |q: &str| {
        investigate::discover::discover(
            &e,
            &root,
            &scope,
            &investigate::discover::terms(q),
            investigate::discover::web(q),
            8,
        )
    };
    assert!(rank(&prompt)[0].path.starts_with("Mcp/"), "the baseline ranking: {:?}", rank(&prompt)[0]);
    let task = items::task(&prompt);
    assert_eq!(task.items.len(), 2, "{task:?}");
    assert_eq!(
        task.constraints,
        [
            "You are running the MCP Proxy Benchmark from /Users/kisi/MCP Proxy Benchmark/fenix.",
            "Use zazzle only and do not edit files.",
            "Answer in a table."
        ]
    );
    let (before, after) = (rank(&prompt), rank(&task.subject()));
    assert_eq!(after[0].path, "Api/Filters/IdempotencyKeyFilter.cs", "{after:?}");
    let decoy = |r: &[investigate::discover::Candidate]| {
        r.iter().find(|c| c.path.starts_with("Mcp/")).map_or(0, |c| c.score)
    };
    assert!(decoy(&after) * 2 <= decoy(&before), "the workspace path no longer lifts the decoy: {after:?}");
}

// Checks idempotency from one unseeded call: both items are about the filter, so one part keeps them both with the whole prompt; discovery reads the filter, not the MCP decoy; the exact malformed `FIND "idempotency" .` finds the filter; an answer resting on the decoy, the subproject guide and the controller's annotation with high confidence is held partial and escalated; the escalation is refused the decoy again, gets the filter from zazzle's lookups, and its answer covers both items, telling the action's `OperationId` header from the filter's `Idempotency-Key` and `X-Operation-Id`.
#[test]
fn test_idempotency_reaches_the_filter() {
    let (_t, root) = fixture(&[]);
    let prompt = format!("{PREAMBLE} (1) Which Django-facing HTTP endpoints implemented in C# require idempotency? (2) How does the idempotency filter treat a blank key, and which headers does it read? Answer in a table.");
    let (c, fl, dc, ag) = (
        "Api/Controllers/DjangoCarWashesFacadeV1Controller.cs",
        "Api/Filters/IdempotencyKeyFilter.cs",
        "Mcp/Tools/FenixLogsReadMachineLogFileTool.cs",
        "Mcp/AGENTS.md",
    );
    let low = format!(
        "ANSWER\nFINDING {dc}:6-7 the MCP proxy tool handles the benchmark's HTTP request\nFINDING {ag}:4 no HTTP endpoint in the repository uses idempotency keys\nFINDING {c}:{} the wash action has the `IdempotencyKeyFilter` attached\nFINDING {fl}:{} the filter reads `OperationId`\nCONFIDENCE understanding=high result=high",
        at(c, "ServiceFilter"),
        at(fl, "Idempotency-Key"),
    );
    let medium = format!(
        "ANSWER\nFINDING {c}:{} the controller's routes start with `api/v1/DjangoCarWashesFacade`\nFINDING {c}:{}-{} the Django-facing wash action requires the `OperationId` header and runs `IdempotencyKeyFilter`\nFINDING {fl}:{}-{} the filter reads `Idempotency-Key` and falls back to `X-Operation-Id`\nFINDING {fl}:{}-{} with a blank key the filter calls `next()` and returns without replay\nCONFIDENCE understanding=high result=high",
        at(c, "[Route("),
        at(c, "[HttpPost"),
        at(c, "public async Task<IActionResult>"),
        at(fl, "Idempotency-Key"),
        at(fl, "X-Operation-Id"),
        at(fl, "X-Operation-Id") + 2,
        at(fl, "X-Operation-Id") + 6,
    );
    let script = [
        ("qwen-a low", Ok("READ Mcp/Tools/FenixLogsReadMachineLogFileTool.cs\nFIND \"idempotency\" .")),
        ("qwen-a low", Ok(low.as_str())),
        ("qwen-a medium", Ok("READ Mcp/Tools/FenixLogsReadMachineLogFileTool.cs:1-11")),
        ("qwen-a medium", Ok(medium.as_str())),
    ];
    let (text, v, f) = run(&root, &prompt, &script);
    assert!(
        text.contains(
            "plan: one part on qwen-a low, no planner call (every item is about IdempotencyKeyFilter)"
        ),
        "{text}"
    );
    assert_eq!(v["plan"]["state"], "skipped");
    let low_asked = f.prompts("qwen-a low");
    assert!(low_asked[0].contains("Question: I1: Which Django-facing HTTP endpoints implemented in C# require idempotency?\nI2: How does the idempotency filter treat a blank key, and which headers does it read?\nConstraints on the answer: You are running the MCP Proxy Benchmark"), "every item and the whole prompt: {}", low_asked[0]);
    assert!(
        low_asked[0].contains(&format!("### E1 READ {fl}:")),
        "discovery reads the filter first, not the decoy: {}",
        low_asked[0]
    );
    assert!(
        low_asked[1].contains("FIND \"idempotency\" .\n") && low_asked[1].contains("IdempotencyKeyFilter"),
        "the quoted FIND finds the filter: {}",
        low_asked[1]
    );
    let first = &v["parts"][0]["attempts"][0];
    let find = first["result"]["diagnostics"]["lookups"]
        .as_array()
        .expect("lookups")
        .iter()
        .find(|l| l["lookup"] == "FIND \"idempotency\" .")
        .cloned()
        .expect("the FIND");
    assert_eq!(
        (find["found"].clone(), find["search"].clone()),
        (json!(true), json!({"text": "idempotency", "case": "any", "within": "."}))
    );
    assert!(
        first["escalated_because"].as_str().is_some_and(|w| w.starts_with("it left I1 unresolved")),
        "{first:#}"
    );
    assert_eq!(first["result"]["status"], "partial", "high confidence does not make it complete");
    let unrelated = first["result"]["unrelated_citations"].to_string();
    assert!(
        unrelated.contains(dc) && unrelated.contains(ag),
        "the decoy and the subproject guide cover nothing: {unrelated}"
    );
    assert!(first["result"]["rejected"].to_string().contains("`OperationId`"), "{first:#}");
    let missing = first["result"]["coverage"][1]["missing"].to_string();
    assert!(missing.contains(fl), "the annotation is not the filter's behaviour: {missing}");
    let med = f.prompts("qwen-a medium");
    assert!(
        med[0].contains(&format!(
            "- READ {dc} — found nothing: what it cited there is about none of the items asked"
        )),
        "{}",
        med[0]
    );
    assert!(
        med[0].contains(&format!("READ {c}:"))
            && med[0].contains("[Route(\"api/v1/DjangoCarWashesFacade\")]"),
        "zazzle reads the route for it: {}",
        med[0]
    );
    assert!(
        med[1].contains(&format!("Not run again: READ {dc}:1-11 (an earlier attempt already made it")),
        "the decoy is not reread: {}",
        med[1]
    );
    assert_eq!(v["status"], "complete", "{v:#}");
    let text = v["answer"]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains(&format!(
            "- About none of the items: no HTTP endpoint in the repository uses idempotency keys ({ag}:4)"
        )),
        "{text}"
    );
    assert!(
        v["coverage"].as_array().expect("coverage").iter().all(|c| c["covered"] == true),
        "{:#}",
        v["coverage"]
    );
    let cites: Vec<String> = v["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|f| f["excerpt"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(cites.iter().any(|e| e.contains("Name = \"OperationId\"")), "{cites:?}");
    assert!(
        cites.iter().any(|e| e.contains("\"Idempotency-Key\"") && e.contains("\"X-Operation-Id\"")),
        "{cites:?}"
    );
    assert!(
        cites.iter().any(|e| e.contains("IsNullOrWhiteSpace") && e.contains("await next();")),
        "{cites:?}"
    );
    let phases: Vec<String> = v["usage"]["phases"]
        .as_array()
        .expect("phases")
        .iter()
        .map(|p| p["phase"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(phases, ["part", "escalation"], "{:#}", v["usage"]);
    assert!(
        v["usage"]["phases"][1]["local"]["output_tokens"].as_u64().is_some_and(|n| n > 0),
        "measured per phase"
    );
    let rec = f.records.lock().expect("lock")[0].clone();
    let counts = |i: usize| {
        (rec["parts"][0]["attempts"][i]["items"].clone(), rec["parts"][0]["attempts"][i]["covered"].clone())
    };
    assert_eq!((counts(0), counts(1)), ((json!(2), json!(0)), (json!(2), json!(2))), "{rec:#}");
    assert!(!rec.to_string().contains("idempotency filter"), "the log keeps no question text");
}

// The trace scenario's prompt: one requested item after the operational preamble.
fn trace_prompt() -> String {
    format!("{PREAMBLE} Trace a subscription wash request from its Django-facing endpoint, with its full route, through the service to persistence and the workflow.")
}

// The Low answer the trace scenario scripts: the right action, sure of itself, one hop short and without the controller's route prefix.
fn trace_low() -> String {
    let c = "Api/Controllers/DjangoCarWashesFacadeV1Controller.cs";
    format!(
        "ANSWER\nFINDING {c}:{}-{} the endpoint is POST `subscriptions/{{subscriptionId}}/wash` and calls `_djangoFacadeService.StartSubscriptionWash`\nUNKNOWN the service implementation is unresolved\nCONFIDENCE understanding=high result=high",
        at(c, "[HttpPost"),
        at(c, "_djangoFacadeService.StartSubscriptionWash"),
    )
}

// Checks a trace from one unseeded call: Low's sure answer is held partial because the call on the service is not followed and the route prefix is not cited; Medium gets the service, the repository and the workflow read by zazzle up front, and its answer with the full route and the controller → service → persistence and workflow chain is complete.
#[test]
fn test_trace_follows_the_call_chain() {
    let (_t, root) = fixture(&[]);
    let (c, svc, repo, wf) = (
        "Api/Controllers/DjangoCarWashesFacadeV1Controller.cs",
        "Core/Services/DjangoFacadeService.cs",
        "Core/Persistence/WashRepository.cs",
        "Core/Workflow/WashWorkflow.cs",
    );
    let medium = format!(
        "ANSWER\nFINDING {c}:{} the controller's routes start with `api/v1/DjangoCarWashesFacade`\nFINDING {c}:{}-{} POST `subscriptions/{{subscriptionId}}/wash` calls `_djangoFacadeService.StartSubscriptionWash`\nFINDING {svc}:{}-{} the service inserts the wash and enqueues it\nFINDING {repo}:{}-{} the repository adds the wash and saves it\nFINDING {wf}:{}-{} the workflow publishes `StartWash`\nCONFIDENCE understanding=high result=high",
        at(c, "[Route("),
        at(c, "[HttpPost"),
        at(c, "_djangoFacadeService.StartSubscriptionWash"),
        at(svc, "public async Task<WashResult>"),
        at(svc, "EnqueueWash"),
        at(repo, "public async Task<Wash>"),
        at(repo, "SaveChangesAsync"),
        at(wf, "public Task EnqueueWash"),
        at(wf, "Queue.Publish"),
    );
    let low = trace_low();
    let script = [("qwen-a low", Ok(low.as_str())), ("qwen-a medium", Ok(medium.as_str()))];
    let (text, v, f) = run(&root, &trace_prompt(), &script);
    assert!(text.contains("plan: one part on qwen-a low, no planner call (one requested item)"), "{text}");
    assert!(f.prompts("qwen-a low")[0].contains(&format!("### E1 READ {c}:")), "discovery reads the action");
    let first = &v["parts"][0]["attempts"][0];
    let missing = first["result"]["coverage"][0]["missing"].to_string();
    assert!(
        missing.contains(&format!("the call `_djangoFacadeService.StartSubscriptionWash(` at {c}:")),
        "{missing}"
    );
    assert!(
        missing.contains(&format!(
            "the route prefix `[Route(\\\"api/v1/DjangoCarWashesFacade\\\")]` at {c}:7 is not cited"
        )),
        "{missing}"
    );
    assert_eq!(first["result"]["status"], "partial", "an explicit gap on the trace is not complete");
    let med = &f.prompts("qwen-a medium")[0];
    for want in [svc, repo, wf] {
        assert!(med.contains(&format!("READ {want}:")), "zazzle follows the chain to {want}: {med}");
    }
    assert!(med.contains(&format!("READ {c}:7-7")), "{med}");
    assert_eq!(v["status"], "complete", "{v:#}");
    assert_eq!(v["coverage"][0]["covered"], true, "{:#}", v["coverage"]);
    let cited: Vec<&str> = v["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|f| f["evidence"][0].as_str())
        .collect();
    for want in [format!("{c}:7"), format!("{svc}:"), format!("{repo}:"), format!("{wf}:")] {
        assert!(cited.iter().any(|x| x.starts_with(&want)), "{want}: {cited:?}");
    }
}

// Checks a trace whose service implementation is not in the repository ends partial with a specific unresolved item: the escalation can cite only the interface, so the call stays unfollowed.
#[test]
fn test_trace_without_the_implementation_is_partial() {
    let (_t, root) = fixture(&["Core/Services/DjangoFacadeService.cs"]);
    let (c, iface) =
        ("Api/Controllers/DjangoCarWashesFacadeV1Controller.cs", "Core/Services/IDjangoFacadeService.cs");
    let medium = format!(
        "ANSWER\nFINDING {c}:{} the controller's routes start with `api/v1/DjangoCarWashesFacade`\nFINDING {c}:{}-{} POST `subscriptions/{{subscriptionId}}/wash` calls `_djangoFacadeService.StartSubscriptionWash`\nFINDING {iface}:{} the service interface declares `StartSubscriptionWash`\nCONFIDENCE understanding=high result=high",
        at(c, "[Route("),
        at(c, "[HttpPost"),
        at(c, "_djangoFacadeService.StartSubscriptionWash"),
        at(iface, "StartSubscriptionWash"),
    );
    let low = trace_low();
    let script = [("qwen-a low", Ok(low.as_str())), ("qwen-a medium", Ok(medium.as_str()))];
    let (_, v, f) = run(&root, &trace_prompt(), &script);
    assert!(
        f.prompts("qwen-a medium")[0].contains(&format!("READ {iface}:")),
        "only the interface is left to read"
    );
    assert_eq!(v["status"], "partial", "{v:#}");
    let missing = v["coverage"][0]["missing"].to_string();
    assert!(
        missing.contains("the call `_djangoFacadeService.StartSubscriptionWash(`")
            && missing.contains("only an interface declaration is cited"),
        "{missing}"
    );
    assert!(
        v["answer"]["unresolved"].to_string().contains("I1 (Trace a subscription wash request"),
        "{:#}",
        v["answer"]
    );
    let last = v["parts"][0]["attempts"].as_array().expect("attempts").last().cloned().expect("an attempt");
    assert!(
        last["escalated_because"]
            .as_str()
            .is_some_and(|w| w.ends_with("not escalated: it was the top of the ladder")),
        "{last:#}"
    );
}

// Checks the cache from one unseeded call: malformed plans fall back to one part that keeps both items with their context; Low's sure answer about the cache alone, with a rejected identifier, is held partial for want of callers and tests; Medium gets the interface's references and the test classes from zazzle and its answer covers the future-timestamp boundary, the caller through the interface with its explicit memory-served metric, and both tests.
#[test]
fn test_cache_finds_callers_and_tests() {
    let (_t, root) = fixture(&[]);
    let prompt = "Workspace: /Users/kisi/MCP Proxy Benchmark/fenix. Use zazzle only. For the terminal poll response cache: (1) when does Recall refuse an entry whose timestamp is in the future; (2) which production code calls the cache, and which tests cover the cache and its caller? Answer in at most 120 words.";
    let (cache, caller, t1, t2) = (
        "Core/Caching/TerminalPollResponseCache.cs",
        "Core/Services/CarWashesService.TerminalPoll.cs",
        "Tests/Core.Tests/TerminalPollResponseCacheTests.cs",
        "Tests/Api.Tests/TerminalPollFromMemoryTests.cs",
    );
    let low = format!(
        "ANSWER\nFINDING {cache}:{}-{} `Remember` stores the response with its time\nFINDING {cache}:{}-{} `Recall` returns the stored response\nFINDING {cache}:{} `Recall` increments `ServedFromMemoryCount`\nUNKNOWN no callers or tests were found\nCONFIDENCE understanding=high result=medium",
        at(cache, "public void Remember"),
        at(cache, "_entries[terminalId]"),
        at(cache, "public bool Recall"),
        at(cache, "return true;"),
        at(cache, "response = entry.Response"),
    );
    let medium = format!(
        "ANSWER\nFINDING {cache}:{}-{} the recall refuses an entry whose time `At` is after `now`\nFINDING {caller}:{}-{} the poll serves a recalled response from `_terminalPollResponseCache` and then calls `IncrementServedFromMemory` itself\nFINDING {t1}:{}-{} a test remembers an entry a minute ahead and expects `Recall` to refuse it\nFINDING {t2}:{}-{} a test expects the caller to count one response served from memory\nCONFIDENCE understanding=high result=high",
        at(cache, "entry.At > now"),
        at(cache, "entry.At > now") + 3,
        at(caller, "_terminalPollResponseCache.Recall"),
        at(caller, "IncrementServedFromMemory"),
        at(t1, "cache.Remember"),
        at(t1, "Assert.False"),
        at(t2, "service.PollTerminal"),
        at(t2, "Assert.Equal"),
    );
    let script = [
        ("qwen-a medium", Ok("PART 1 | (1) future boundary |\nCONFIDENCE understanding=high result=high")),
        ("qwen-a medium", Ok("PART 1 | I1 |")),
        ("qwen-a low", Ok(low.as_str())),
        ("qwen-a medium", Ok(medium.as_str())),
    ];
    let (text, v, f) = run(&root, prompt, &script);
    assert!(text.contains("the task runs as one assignment on qwen-a low"), "{text}");
    assert_eq!(v["plan"]["state"], "fallback");
    let notes = v["plan"]["notes"].to_string();
    assert!(
        notes.contains("`(1) future boundary` is not an item") && notes.contains("I2 is in no part"),
        "the refusals are kept: {notes}"
    );
    let asked = &f.prompts("qwen-a low")[0];
    assert!(asked.contains("Question: Context: For the terminal poll response cache:\nI1: when does Recall refuse an entry whose timestamp is in the future\nI2: which production code calls the cache, and which tests cover the cache and its caller?\nConstraints on the answer: Workspace: /Users/kisi/MCP Proxy Benchmark/fenix. Use zazzle only. Answer in at most 120 words."), "{asked}");
    let first = &v["parts"][0]["attempts"][0];
    assert!(first["result"]["rejected"].to_string().contains("ServedFromMemoryCount"), "{first:#}");
    assert_eq!(first["result"]["status"], "partial");
    let missing = first["result"]["coverage"][1]["missing"].to_string();
    assert!(missing.contains("no test evidence") && missing.contains("no caller: no finding in production code that uses TerminalPollResponseCache or its interface ITerminalPollResponseCache"), "{missing}");
    let med = &f.prompts("qwen-a medium")[2];
    assert!(
        med.contains("REFS ITerminalPollResponseCache") && med.contains(caller),
        "the interface leads to the caller: {med}"
    );
    assert!(
        med.contains(&format!("READ {t1}:")) && med.contains(&format!("READ {t2}:")),
        "zazzle reads the tests: {med}"
    );
    assert_eq!(v["status"], "complete", "{v:#}");
    let excerpts: Vec<String> = v["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|f| f["excerpt"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(excerpts.iter().any(|e| e.contains("entry.At > now")), "the boundary");
    assert!(
        excerpts
            .iter()
            .any(|e| e.contains(".Recall(") && e.contains("_metrics.IncrementServedFromMemory();")),
        "the recall and the caller's own metric"
    );
    assert_eq!(
        v["usage"]["by_role"].as_array().map(Vec::len),
        Some(3),
        "planner, part and escalation are measured apart: {:#}",
        v["usage"]
    );
}

// Checks a one-rung ladder asks the same agent again with zazzle's lookups when it verified something but left the trace open, and that the second answer settles it.
#[test]
fn test_top_rung_retries_with_lookups() {
    let (_t, root) = fixture(&[]);
    let (c, svc) =
        ("Api/Controllers/DjangoCarWashesFacadeV1Controller.cs", "Core/Services/DjangoFacadeService.cs");
    let second = format!(
        "ANSWER\nFINDING {c}:{} the controller's routes start with `api/v1/DjangoCarWashesFacade`\nFINDING {c}:{}-{} POST calls `_djangoFacadeService.StartSubscriptionWash`\nFINDING {svc}:{}-{} the service inserts and enqueues the wash\nFINDING Core/Persistence/WashRepository.cs:{}-{} the repository saves it\nFINDING Core/Workflow/WashWorkflow.cs:{}-{} the workflow publishes it\nCONFIDENCE understanding=high result=high",
        at(c, "[Route("),
        at(c, "[HttpPost"),
        at(c, "_djangoFacadeService.StartSubscriptionWash"),
        at(svc, "public async Task<WashResult>"),
        at(svc, "EnqueueWash"),
        at("Core/Persistence/WashRepository.cs", "public async Task<Wash>"),
        at("Core/Persistence/WashRepository.cs", "SaveChangesAsync"),
        at("Core/Workflow/WashWorkflow.cs", "public Task EnqueueWash"),
        at("Core/Workflow/WashWorkflow.cs", "Queue.Publish"),
    );
    let low = trace_low();
    let again = low.lines().find(|l| l.starts_with("FINDING")).expect("a finding");
    let second = second.replacen("ANSWER\n", &format!("ANSWER\n{again}\n"), 1);
    let f = Fake::new(&[("qwen-a low", Ok(low.as_str())), ("qwen-a low", Ok(second.as_str()))]);
    let argv = [trace_prompt(), "--adaptive".into(), "--agents".into(), "qwen-a@low".into(), "--json".into()];
    let v: Value = serde_json::from_str(
        &go(&root, &f, &argv.iter().map(String::as_str).collect::<Vec<_>>()).expect("run"),
    )
    .expect("json");
    let why = v["parts"][0]["attempts"][0]["escalated_because"].as_str().unwrap_or_default().to_string();
    assert!(why.ends_with("; asked again with zazzle's lookups"), "{why}");
    assert!(
        f.prompts("qwen-a low")[1].contains(&format!("READ {svc}:")),
        "the retry starts from zazzle's lookups"
    );
    assert_eq!(v["status"], "complete", "{v:#}");
}

// Checks the coverage of items whose evidence falls short in other ways: a trace that stays in one file, a route with no attribute cited, callers of a class with no interface, and tests with nothing verified.
#[test]
fn test_coverage_shortfalls() {
    let (_t, root) = fixture(&[]);
    let e = crate::engine::Engine::new();
    let ctx = Ctx {
        engine: &e,
        cwd: root.clone(),
        session: String::new(),
        roots: Vec::new(),
        stdin: RefCell::new(None),
        caller: RefCell::new(None),
    };
    let scope = [root.clone()];
    let found = |path: &str, from: usize, to: usize| {
        let c = investigate::cite::Claim { path: path.into(), start: from, end: to, text: "a claim".into() };
        investigate::verify(&root, &scope, &c).0.expect("a verified finding")
    };
    let (svc, c, dc) = (
        "Core/Services/DjangoFacadeService.cs",
        "Api/Controllers/DjangoCarWashesFacadeV1Controller.cs",
        "Mcp/Tools/FenixLogsReadMachineLogFileTool.cs",
    );
    let task = items::task("1. Trace the Django facade service flow\n2. Which endpoint routes the subscription wash?\n3. Which code calls the Fenix logs read machine log file tool?\n4. Which tests cover the blank wash bay timer?");
    let one = |i: usize, f: Vec<investigate::Found>| {
        coverage::check(&ctx, &scope, &task, &[task.items[i].id.clone()], &f)
    };
    let trace = one(0, vec![found(svc, at(svc, "return new WashResult"), at(svc, "return new WashResult"))]);
    assert!(
        trace.items[0]
            .missing
            .join("; ")
            .starts_with(&format!("the trace stays in {svc}; the calls it makes are not followed")),
        "{trace:?}"
    );
    assert!(
        trace.follow.iter().any(|l| l.starts_with("READ Core/Persistence/WashRepository.cs:")),
        "{trace:?}"
    );
    let route = one(1, vec![found(c, at(c, "var commandContext"), at(c, "var commandContext"))]);
    assert!(route.items[0].missing.contains(&"no route attribute is cited".to_string()), "{route:?}");
    let callers = one(2, vec![found(dc, at(dc, "return Fenix.Logs.Read"), at(dc, "return Fenix.Logs.Read"))]);
    let missing = callers.items[0].missing.join("; ");
    assert!(
        missing
            .contains("no caller: no finding in production code that uses FenixLogsReadMachineLogFileTool")
            && !missing.contains("interface"),
        "{missing}"
    );
    let tests = one(3, Vec::new());
    assert!(tests.items[0].missing.iter().any(|m| m.contains("no test evidence")), "{tests:?}");
    let decoy = one(1, vec![found(dc, at(dc, "[HttpPost"), at(dc, "public string"))]);
    assert!(
        !decoy.items[0].missing.iter().any(|m| m.contains("prefix")),
        "a file with no class route has no prefix to cite: {decoy:?}"
    );
    let prefix = found(c, at(c, "[Route("), at(c, "[Route("));
    let routed = one(1, vec![found(c, at(c, "[HttpPost"), at(c, "public async")), prefix]);
    assert!(routed.items[0].missing.is_empty(), "{routed:?}");
    let task = items::task("1. Which tests cover the terminal poll response cache?\n2. Which tests cover the terminal poll response cache recall?");
    let t1 = "Tests/Core.Tests/TerminalPollResponseCacheTests.cs";
    let both = coverage::check(
        &ctx,
        &scope,
        &task,
        &task.ids(),
        &[found(t1, at(t1, "cache.Remember"), at(t1, "cache.Remember"))],
    );
    assert!(both.items.iter().all(|i| i.missing.is_empty()), "{both:?}");
    let none = coverage::check(&ctx, &scope, &task, &task.ids(), &[]);
    let reads = none.follow.iter().filter(|l| l.contains(t1)).count();
    assert_eq!(reads, 1, "a lookup two items need is asked once: {none:?}");
}
