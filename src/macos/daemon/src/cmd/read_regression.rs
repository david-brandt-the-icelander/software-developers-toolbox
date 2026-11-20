// Regression tests for literal reads: one read exposes what a task needs (SQL predicates, branches, attributes, documentation, whole methods), says so when it is cut, and reads the same through the CLI and MCP.
use crate::testutil::{run_cmd, TempDir};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

// A repository whose SQL keeps its eligibility and expiry predicates in a long string.
const REPOSITORY: &str = r#"namespace Shop.Orders;

using System.Data;

// Loads orders that may still be fulfilled.
public sealed class OrderRepository
{
    private const string EligibleOrdersSql = @"
        SELECT o.id, o.customer_id, o.total
        FROM orders o
        WHERE o.status IN ('paid', 'authorized')
          AND o.fulfilment_hold = 0
          AND o.reserved_until > SYSUTCDATETIME()
          AND o.expires_at > DATEADD(minute, -15, SYSUTCDATETIME())
        ORDER BY o.created_at";

    public IEnumerable<Order> Eligible(IDbConnection db) => db.Query<Order>(EligibleOrdersSql);
}
"#;

// Builds an idempotency handler whose resolution and rejection branches sit in the middle of a long method.
fn handler() -> String {
    let mut lines = vec![
        "namespace Shop.Api;".to_string(),
        String::new(),
        "public sealed class IdempotencyFilter".to_string(),
        "{".to_string(),
        "    public async Task<IActionResult> Resolve(HttpRequest request, IStore store)".to_string(),
        "    {".to_string(),
    ];
    for i in 0..40 {
        lines.push(format!("        var trace{i} = request.HttpContext.TraceIdentifier + \"-{i}\";"));
    }
    lines.extend(
        [
            "        var key = request.Headers[\"Idempotency-Key\"].FirstOrDefault()",
            "            ?? request.Headers[\"X-Idempotency-Key\"].FirstOrDefault();",
            "        if (string.IsNullOrWhiteSpace(key))",
            "            return new BadRequestObjectResult(\"missing Idempotency-Key\");",
            "        if (key.Length > 64)",
            "            return new BadRequestObjectResult(\"Idempotency-Key longer than 64\");",
            "        var previous = await store.Find(key);",
            "        if (previous is { BodyHash: var h } && h != request.BodyHash())",
            "            return new ConflictObjectResult(\"key reused with a different body\");",
        ]
        .map(String::from),
    );
    for i in 0..40 {
        lines.push(format!("        await store.Touch(key, {i});"));
    }
    lines.extend(["        return new OkResult();", "    }", "}"].map(String::from));
    lines.join("\n") + "\n"
}

// A controller whose attributes and documentation are the contract a caller relies on.
const CONTROLLER: &str = r#"namespace Shop.Api;

[ApiController]
[Route("api/v2/orders")]
public sealed class OrdersController : ControllerBase
{
    /// <summary>Creates an order once per Idempotency-Key.</summary>
    /// <response code="201">The order was created.</response>
    /// <response code="409">The key was used with a different body.</response>
    [HttpPost]
    [Authorize(Policy = "orders:write")]
    [ProducesResponseType(typeof(OrderDto), StatusCodes.Status201Created)]
    [ProducesResponseType(StatusCodes.Status409Conflict)]
    public async Task<IActionResult> Create([FromBody] CreateOrder body, [FromHeader(Name = "Idempotency-Key")] string key)
    {
        var order = await _orders.Create(body, key);
        return CreatedAtAction(nameof(Get), new { id = order.Id }, order);
    }
}
"#;

// Writes the fixture repository.
fn fixture() -> TempDir {
    let t = TempDir::new();
    t.write("OrderRepository.cs", REPOSITORY.as_bytes());
    t.write("IdempotencyFilter.cs", handler().as_bytes());
    t.write("OrdersController.cs", CONTROLLER.as_bytes());
    t
}

// An MCP backend that runs each command in the fixture, as daemon would.
struct Local(PathBuf);

impl crate::mcp::Backend for Local {
    // Runs the command line and returns its status and text.
    fn call(&self, args: Vec<String>, _session: Option<&str>) -> Result<(u8, String), String> {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = run_cmd(&self.0, &refs);
        Ok((o.status, o.text))
    }
}

// Calls zazzle_read over MCP in the fixture and returns its text.
fn mcp_read(dir: &Path, args: Value) -> String {
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "zazzle_read", "arguments": args}});
    let reply = crate::mcp::handle_line(&req.to_string(), &Local(dir.to_path_buf())).expect("reply");
    reply["result"]["content"][0]["text"].as_str().expect("text").to_string()
}

// Checks one read of the repository shows the SQL eligibility and expiry predicates verbatim.
#[test]
fn test_read_shows_sql_predicates() {
    let t = fixture();
    let o = run_cmd(t.path(), &["read", "OrderRepository.cs"]).text;
    for want in [
        "WHERE o.status IN ('paid', 'authorized')",
        "AND o.fulfilment_hold = 0",
        "AND o.reserved_until > SYSUTCDATETIME()",
        "AND o.expires_at > DATEADD(minute, -15, SYSUTCDATETIME())",
        "using System.Data;",
        "// Loads orders that may still be fulfilled.",
    ] {
        assert!(o.contains(want), "{want}: {o}");
    }
    assert!(
        o.starts_with("OrderRepository.cs 18 lines, sha ") && !o.contains("INCOMPLETE") && !o.contains('⋯'),
        "{o}"
    );
}

// Checks one symbol read returns the whole handler, with the header resolution and every rejection branch from its middle.
#[test]
fn test_read_symbol_keeps_middle_branches() {
    let t = fixture();
    let o = run_cmd(t.path(), &["read", "IdempotencyFilter.cs", "--symbol", "Resolve"]).text;
    for want in [
        "request.Headers[\"Idempotency-Key\"].FirstOrDefault()",
        "?? request.Headers[\"X-Idempotency-Key\"].FirstOrDefault();",
        "return new BadRequestObjectResult(\"missing Idempotency-Key\");",
        "return new BadRequestObjectResult(\"Idempotency-Key longer than 64\");",
        "return new ConflictObjectResult(\"key reused with a different body\");",
        "return new OkResult();",
    ] {
        assert!(o.contains(want), "{want}: {o}");
    }
    let body = o.lines().filter(|l| l.contains('\t')).count();
    assert_eq!(body, 93, "the whole method, lines 5-97: {o}");
    assert!(
        o.lines().next().expect("header").contains("[5-97 method Resolve]") && !o.contains("INCOMPLETE"),
        "{o}"
    );
}

// Checks one symbol read returns the action with its attributes and its contract documentation.
#[test]
fn test_read_symbol_keeps_attributes_and_docs() {
    let t = fixture();
    let o = run_cmd(t.path(), &["read", "OrdersController.cs", "--symbol", "Create"]).text;
    for want in [
        "/// <summary>Creates an order once per Idempotency-Key.</summary>",
        "/// <response code=\"409\">The key was used with a different body.</response>",
        "[HttpPost]",
        "[Authorize(Policy = \"orders:write\")]",
        "[ProducesResponseType(typeof(OrderDto), StatusCodes.Status201Created)]",
        "[FromHeader(Name = \"Idempotency-Key\")] string key",
        "return CreatedAtAction(nameof(Get), new { id = order.Id }, order);",
    ] {
        assert!(o.contains(want), "{want}: {o}");
    }
    let class = run_cmd(t.path(), &["read", "OrdersController.cs", "--symbol", "OrdersController"]).text;
    assert!(class.contains("[ApiController]") && class.contains("[Route(\"api/v2/orders\")]"), "{class}");
}

// Checks a capped read says it is incomplete, names the omitted lines and the read that continues it, and the continuation completes it.
#[test]
fn test_read_capped_says_how_to_continue() {
    let t = fixture();
    let o =
        run_cmd(t.path(), &["read", "IdempotencyFilter.cs", "--symbol", "Resolve", "--budget", "200"]).text;
    let header = o.lines().next().expect("header");
    assert!(header.contains("[5-97 method Resolve], INCOMPLETE: lines 5-"), "{header}");
    let footer = o.lines().last().expect("footer");
    let next = footer.rsplit("continue with: zazzle read ").next().expect("continuation");
    assert!(
        footer.starts_with("-- incomplete: ")
            && next.starts_with("IdempotencyFilter.cs:")
            && next.ends_with("-97"),
        "{footer}"
    );
    let rest = run_cmd(t.path(), &["read", next]).text;
    assert!(rest.contains("return new OkResult();") && !rest.contains("INCOMPLETE"), "{rest}");
}

// Checks the CLI and MCP return the same literal text for a range and a symbol, and terse stays an explicit opt-in on both.
#[test]
fn test_read_cli_and_mcp_agree() {
    let t = fixture();
    let cli = run_cmd(t.path(), &["read", "OrderRepository.cs:7-15"]).text;
    assert_eq!(mcp_read(t.path(), json!({"paths": ["OrderRepository.cs:7-15"]})), cli);
    let sym = run_cmd(t.path(), &["read", "OrdersController.cs", "--symbol", "Create"]).text;
    assert_eq!(mcp_read(t.path(), json!({"paths": ["OrdersController.cs"], "symbol": "Create"})), sym);
    let source = mcp_read(t.path(), json!({"paths": ["OrderRepository.cs"], "source": true}));
    assert_eq!(source, run_cmd(t.path(), &["read", "OrderRepository.cs"]).text, "source=true is the default");
    let capped = mcp_read(t.path(), json!({"paths": ["IdempotencyFilter.cs"], "budget": 100000}));
    assert!(!capped.contains("INCOMPLETE") && capped.contains("return new OkResult();"), "{capped}");
}

// Checks two uneven ranges (about 8,800 and 1,400 tokens) come back whole under a 16,000 budget in either order, and a selection over the budget still returns a prefix with its continuation.
#[test]
fn test_read_uneven_ranges_share_budget() {
    let t = TempDir::new();
    let big: String =
        (1..=540).map(|i| format!("        var value{i} = Compute(input, {i}, \"field-{i}\");\n")).collect();
    let small: String =
        (1..=90).map(|i| format!("        var other{i} = Compute(input, {i}, \"x-{i}\");\n")).collect();
    t.write("Big.cs", big.as_bytes());
    t.write("Small.cs", small.as_bytes());
    let est = |a: &[&str]| crate::tokens::estimate(&run_cmd(t.path(), a).text);
    let (b, s) = (est(&["read", "Big.cs:1-540", "--budget", "16000"]), est(&["read", "Small.cs:1-90"]));
    assert!(b > 7000 && b < 12000 && s > 1000 && s < 3000, "{b} {s}");
    for order in [["Big.cs:1-540", "Small.cs:1-90"], ["Small.cs:1-90", "Big.cs:1-540"]] {
        let o = run_cmd(t.path(), &["read", order[0], order[1], "--budget", "16000"]).text;
        assert!(!o.contains("INCOMPLETE") && o.contains("value540") && o.contains("other90"), "{o}");
        assert!(o.find(&order[0][..3]) < o.find(&order[1][..3]), "kept in the asked order");
    }
    let o = run_cmd(t.path(), &["read", "Big.cs:1-540", "Small.cs:1-90", "--budget", "6000"]).text;
    assert!(
        o.contains("other90") && o.contains("INCOMPLETE") && o.contains("continue with: zazzle read Big.cs:"),
        "{o}"
    );
}
