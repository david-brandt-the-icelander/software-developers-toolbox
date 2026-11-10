# Python rules for agents

Sources: PEP 8, PEP 484, Google Python Style Guide, docs.python.org venv tutorial. Condensed; repo rules win on conflict.

## Environment
- Always use a venv: `python3 -m venv .venv && source .venv/bin/activate`; never install into system Python.
- Run tools as `python -m pip` / `python -m pytest` so the venv's copy runs.
- Pin deps in `requirements.txt` (`python -m pip freeze > requirements.txt`); commit it, never `.venv/`.
- Unset `PYTHONPATH`. Run untrusted files with `python -I`.
- Prefer stdlib; add a dependency only with a stated reason.
- Scripts meant for stock macOS Python 3.9: start with `from __future__ import annotations`, avoid `match` and 3.10+ runtime syntax.

## Comments (repo rule)
- Every file, class and function gets exactly one `#` comment: one plain-English sentence saying what it is for. Files: first line after the shebang. Classes/functions: the line above `def`/`class`.
- No docstrings, no Args/Returns sections, no XML/Sphinx/pydoc metadata.
- Other comments only for a non-obvious why; never restate the code. Keep them true.

## Token-lean, agent-readable code
- Small single-purpose functions; descriptive names so the comment stays short.
- Flat control flow: early return/continue over nesting.
- No dead code, no commented-out code, no speculative options.
- Explicit over clever; no metaclasses, import hacks, reflection, `__del__`.
- Group a file top-down: imports, constants, helpers, commands, `main()`.
- Keep files under ~800 lines; split by responsibility.
- Print concise output; paths and numbers, not prose.

## Layout and naming
- 4 spaces, UTF-8, no trailing whitespace; lines <= 100 chars; wrap in brackets, not backslashes.
- Two blank lines around top-level defs, one between methods.
- Modules `lower_snake.py` (no dashes); classes `CapWords`; functions/variables `snake_case`; constants `UPPER_SNAKE`; internal names `_leading`.
- No `l`, `O`, `I` names. No abbreviations except loop counters and `e`/`f`.
- One statement per line; trailing comma when closing bracket is on its own line.

## Imports
- Top of file, grouped stdlib / third-party / local, alphabetical, blank line between groups.
- One import per line; no `import *`; absolute imports.
- No import-time side effects; put logic in `main()` behind `if __name__ == "__main__":`.

## Typing
- Annotate public function signatures; `-> None` explicit.
- Built-in generics (`list[int]`), `X | None` (with `__future__` annotations on 3.9); never `a: str = None`.
- Accept abstract types (`Sequence`, `Mapping`), return concrete ones.
- Avoid `Any`; use `# type: ignore[code]` sparingly.

## Errors
- Never bare `except:`; catch the narrowest exception, keep `try` bodies minimal.
- Re-raise with `raise X from Y`; custom errors subclass `Exception`, named `...Error`.
- `assert` only in tests; validate input with explicit raises.
- Use `with` for files/locks/sockets; `finally` for cleanup; no `return` in `finally`.
- Subprocesses: list args, never `shell=True`, always a timeout.

## Functions and data
- No mutable default args (`None` then create).
- `is None` for None checks; `if seq:` for emptiness; `isinstance` not `type(x) is`.
- Named `def` over assigned lambdas; comprehensions only when simple; generators for large data.
- No `@staticmethod` (use module function); `@property` only for trivial reads.
- Build strings with `''.join` / f-strings; log with constant format + args.
- Use `pathlib` or `os.path` consistently; never follow symlinks implicitly when deleting.

## Testing
- pytest, plain `assert`, tests named `test_<unit>_<case>`; keep logic at module level so it is testable.
- Run the fast checks on changed code before committing.
