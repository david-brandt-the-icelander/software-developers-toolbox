# C# rules for agents

## Toolchain
- .NET SDK matching `TargetFramework` in `Directory.Build.props`; warnings are errors.
- Prefer the BCL; add a NuGet package only with a stated reason.

## Comments (repo rule)
- Every file, type, method and test gets exactly one `//` comment: one plain-English sentence saying what it is for. Files: first line. Items: the line above (above attributes).
- No XML doc comments. Other comments only for a non-obvious why.

## Token-lean, agent-readable code
- Small single-purpose methods; flat control flow with early `return`/`continue`.
- No dead code, no `#pragma warning disable`, no speculative options.
- Keep files under ~800 lines.
- No reflection-based JSON (`JsonSerializer` over types); use `JsonNode` so trimming stays safe.

## Errors and safety
- Subprocesses: `Proc.Run` with an argument list, never a shell string, always a timeout.
- Resolve every agent-supplied path through `Root.Resolve`.

## Testing
- xunit tests in `tests/`, named `test_<unit>_<case>`; tests must pass on Linux and macOS.
