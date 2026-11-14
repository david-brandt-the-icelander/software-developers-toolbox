# zazzle / zazzled

A file tool for agents: `zazzle` replaces `grep`, `cat`, `sed -n`, `find`, `ls` and `sed -i` with commands whose output is short, capped, ranked and deduplicated per session.

- `zazzle` (`macos/c/zazzle/`): a tiny C program with no libc on Linux (raw syscalls); on macOS it links libSystem but still uses raw syscalls for all I/O. It sends its arguments, working-directory descriptor and standard streams to the daemon and exits with the daemon's status byte.
- `zazz`: the same invoker under another name, for the zazz script commands below; installed scripts are symlinks to it.
- `zazzled` (this crate): the long-lived daemon. `zazzled serve` listens on `~/.zazzle/sock`; `zazzled mcp` is an MCP stdio server in front of the same daemon (no process spawn per call); `zazzled run CMD..` runs one command in-process; `zazzled proxy` is the MCP proxy that `zazzle proxy` execs ([MCP proxy](#mcp-proxy)); `zazzled install` / `uninstall` manage the macOS LaunchAgent.

## Setup

```bash
cd macos/rust && cargo build --release -p zazzled && cd ../c/zazzle && make     # or download the release tarball
zazzled install                       # macOS: launchd starts the daemon on demand (elsewhere zazzle starts it itself)
export ZAZZLE_SESSION=$(date +%s)     # optional: ids and dedupe for CLI use
claude mcp add zazzle -- zazzled mcp     # MCP: tools zazzle_find, zazzle_read, zazzle_ls, zazzle_outline, zazzle_refs, zazzle_edit, zazzle_read_again, and the C# structure tools below
```

Any MCP client works with `{"mcpServers":{"zazzle":{"command":"zazzled","args":["mcp"]}}}`; the working directory of the server is the root for relative paths and the only place edits may write.

## Commands

| command | output |
|---|---|
| `find PATTERN [PATH..] [-i -F -w -c -l -u] [-C N] [-g GLOB] [-m N]` | grouped by file, `N:` match and `N-` context lines, long lines clipped around the match, capped (50) with a footer saying how to narrow; ordinary files before deep files before tests |
| `read PATH[:A-B].. [--source --symbol NAME[#N] --around N --ctx N --head N --tail N --budget T --anchors --full]` | header with line count and sha, then `N<TAB>text`; elides the middle beyond ~3000 tokens with the exact range to read next; `--symbol` reads one symbol with its doc comments and attributes (see [Symbols](#symbols)); C# files come as a terse view (see [C# terse view](#c-terse-view)) unless `--source` |
| `ls [DIR] [-d N] [-m N]` | tree with recursive file counts and line counts |
| `outline PATH.. [-d N] [-m N]` | `start-end kind name` per symbol; tree-sitter for Rust, Python, C, C#, JS/TS/TSX up to 1 MiB; line-based for larger files, Markdown, TOML, JSON and everything else |
| `refs SYMBOL [PATH..]` | definitions first, then uses with their enclosing symbol; in C# files a mention in a comment or string is not a use (tree-sitter) |
| `edit PATH ...` | `--old/--new` unique replace, `--lines` or `--after`, each with `--expect SHA` (from `read --source`) or `--at` anchors; `--create`; atomic, CRLF-preserving, confined to the working directory |
| `run [--lossless --budget T --timeout SECS --profile NAME\|none] [--] CMD ARGS..` | runs a command and prints its output cleaned with a profile, then `[exit N in Ts; …]`; exits with the command's code: see [Run](#run) |
| `proxy ...` | runs the MCP proxy as its own process: see [MCP proxy](#mcp-proxy) |
| `CMD \| clean [--as auto\|grep\|json\|text] [--lossless] [--budget T] [--stats]` | the piped output of any command, shrunk: see [Clean](#clean) |
| `overview`, `find-symbol`, `changed-since`, `git-log`, `git-blame`, `report-gap` | structure answered by `zazzle-csharp-indexer`: see [C# structure](#c-structure) |
| `status` | daemon, cache and session counters |

Any command accepts `--budget T` (caps output at about T tokens, default 8000; `read` and `clean` have their own) and `--disable-session-deduping`.

## C# terse view

`zazzle read FILE.cs` returns a terse behavioural view from `zazzle-csharp-indexer` (Roslyn) instead of the code, and no sha. `--source` (or `--anchors`) returns the code and its sha, which every edit needs.

- Hidden or collapsed: using blocks (`1-12 ⋯ 12 using(s)`), logging and tracing statements on loggers, `Debug`/`Trace` and spans with no side effects in their arguments, `if (logger.IsEnabled(..))` blocks that only log, `#region` lines, constructors that only assign fields (`13-18 ⋯ ctor(ILogger<X> logger, IRepo repo): only assigns fields`), and catch clauses that only log and `throw;`.
- Shortened: `public`/`private`/`protected`/`internal` on declarations, `var` where the type is spelled out on the right (`new T(..)`, `(T)x`, `x as T`), and local variables of 6+ characters, to the initials of their words (`invoiceRepository` → `ir`) or three letters of one word. Locals are bound by a compilation of the file, so fields, members and parameters with the same name keep theirs; a clash or keyword gets a number; a local used in `nameof` keeps its name. Parameters are never renamed.
- Kept: everything else, including catches that return or convert, constructors with logic, and logging that shares a line with other code. Lines keep their source numbers: `N` unchanged, `N~` changed, `A-B ⋯ note` hidden. Ranges, `--symbol` and the budget work on the view.
- The bridge: `zazzled` connects to the indexer daemon's socket (in `$ZAZZLE_INDEXER_STATE_DIR` or the indexer's default state directory), starting `zazzle-csharp-indexer daemon` when nothing listens, and asks its `view` tool. The indexer is `$ZAZZLE_INDEXER`, else beside `zazzled`, else on `PATH`. The file must be inside a git work tree. When there is no view (no indexer, not in git, an error), the source is returned with a line saying why. The first read starts the daemon (about 0.5 s); later ones take a few milliseconds.

## HTML and Razor terse view

`zazzle read` on `.html`, `.htm`, `.xhtml`, `.cshtml` and `.razor` files returns a terse view built inside `zazzled`, in the same line format and with no sha; `--source` returns the code. A file with nothing to shorten comes back as source with its sha.

- Hidden: comments of 3+ lines (`<!-- -->`, Razor `@* *@`), `<style>` and `<svg>` elements that start and end their own lines, runs of 2+ `<meta>`/`<link>`/`<base>` lines, and in Razor runs of 2+ `@using`/`@addTagHelper`/`@removeTagHelper` lines.
- Shortened: `class` values with more than 4 classes to their first 3 and `…`, `style` values over 40 characters to `…`; values containing `@`, `{{` or `${` are left alone.
- Razor `@code` and `@functions` blocks: the C# inside gets the C# terse view from `zazzle-csharp-indexer`, as a `.cs` file would (zazzled sends the blocks as text padded to the file's line numbers, so the view keeps them); without the indexer or a git work tree they are shown as they are.
- Shown as they are: `<script>`, `<pre>` and `<textarea>` blocks, and Razor `@{ }` blocks (they mix C# statements with markup). A comment or element that shares its first or last line with other markup is not hidden.

## CSS, SCSS, Sass and Less terse view

`zazzle read` on `.css`, `.scss`, `.sass` and `.less` files returns a terse view built inside `zazzled` (`src/styles.rs`), line by line, in the same line format and with no sha; `--source` returns the code. A file with nothing to shorten comes back as source with its sha.

- Hidden: `/* */` comments of 4+ lines and SCSS `//` runs of 4+ lines (shorter ones are usually section titles), `/*# sourceMappingURL */` comments, runs of 2+ `@import`/`@use`/`@forward` lines, `@font-face` blocks (`7-10 ⋯ @font-face "Inter", 4 line(s)`) and `@keyframes` blocks, vendor-prefixed declarations (`-webkit-`, `-moz-`, `-ms-`, `-o-`) when the same block also sets the plain property, and SCSS and Sass `@debug` lines. Less is read like SCSS; indented Sass finds its blocks (comments, `@font-face`, `@keyframes`, the block a prefixed declaration belongs to) by indentation and needs no `;`.
- Shortened: `url(data:…)` values longer than 60 characters.
- Kept: everything else; a comment or block that shares its first or last line with other rules is not hidden, and minified files gain little.

## JSON, YAML, TOML and Markdown terse view

`zazzle read` on `.json`, `.jsonc`, `.json5`, `.yaml`, `.yml`, `.toml`, `.md`, `.markdown` and `.mdx` files, and on `Cargo.lock`, `poetry.lock`, `uv.lock`, `pdm.lock`, `composer.lock` and `Pipfile.lock`, returns a terse view built line by line (`src/data.rs`, `src/md.rs`), in the same line format and with no sha; `--source` returns the file. A file with nothing to shorten comes back as source with its sha.

- Lists: a run of 10+ sibling items in a JSON array, a YAML sequence or a TOML array keeps its first and last items and collapses the rest (`5-60 ⋯ 18 item(s)`). Items that span lines collapse only when they all name the same keys, so CI steps and other lists of different things stay. Runs nested in a hidden item go with it.
- Lock files (`*.lock`, `*-lock.json`, `*-lock.yaml`, `*.lock.json`): any 10+ sibling entries collapse the same way (`⋯ 365 entries`), so a `package-lock.json` reads as its first and last packages.
- TOML: a run of 6+ `[[name]]` tables with the same name keeps its first and last (`⋯ 310 [[package]] table(s)` in `Cargo.lock`).
- Hidden: `#` comment blocks (`//` in JSON with comments) of 4+ lines; YAML block scalars (`key: |`, `- >`) and TOML multi-line strings of 6+ lines keep their opening line (`⋯ text, 12 line(s)`). Quoted strings longer than 120 characters keep their first 40 (`N~`).
- Markdown: HTML comments (multi-line ones as `⋯ comment, N line(s)`), lines made only of badge images (shields.io, badgen, codecov, coveralls, `badge` or `.svg` sources), runs of 2+ link reference definitions (`[x]: url`) and lists of 3+ entries that only link to anchors (tables of contents). Fenced code and front matter are left alone.
- Kept: everything else; minified JSON (one long line) comes back as source.

## XML, INI and Dockerfile terse view

`zazzle read` on XML files (`.xml`, `.csproj`, `.fsproj`, `.vbproj`, `.vcxproj`, `.props`, `.targets`, `.xaml`, `.axaml`, `.plist`, `.resx`, `.xsd`, `.xsl`, `.xslt`, `.wsdl`, `.nuspec`, `.manifest`), INI-style files (`.ini`, `.cfg`, `.properties`, `.editorconfig`) and Dockerfiles (`Dockerfile`, `Dockerfile.*`, `*.dockerfile`, `Containerfile`) returns a terse view built line by line (`src/xml.rs`, `src/config.rs`), with no sha; `--source` returns the file.

- XML: comments of 3+ lines (`⋯ comment, N line(s)`); runs of 10+ sibling elements with the same tag, one per line or each closing at the same indentation, keep their first and last (`⋯ 40 <PackageReference> element(s)`); quoted values over 120 characters and runs of 120+ base64-style characters keep their first 40 (`N~`).
- INI: `;` and `#` comment blocks of 4+ lines, and long values as in XML.
- Dockerfile: `#` comment blocks of 4+ lines (`# syntax=` and `# escape=` directives stay), runs of 3+ `LABEL` lines (or one `LABEL` continued over 3+ lines), and 6+ package names one per continued line after a `RUN … \` line (`⋯ 25 package(s)`).
- Kept: everything else, including short comments, mixed tags and commands between packages.

## Python, JavaScript, TypeScript, Dart, Rust, Go, Java, Kotlin, Swift, C, C++, Ruby, PHP, Scala, Perl, Lua, Elixir, Haskell, OCaml, F#, Clojure, Erlang, R, Julia, Zig, Objective-C, Groovy, PowerShell, SQL, Visual Basic, Nim and shell terse view

`zazzle read` on `.py`, `.pyi`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.ts`, `.mts`, `.cts`, `.tsx`, `.dart`, `.rs`, `.go`, `.java`, `.kt`, `.kts`, `.swift`, `.c`, `.h`, `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx`, `.rb`, `.rake`, `.php`, `.scala`, `.sc`, `.pl`, `.pm`, `.lua`, `.ex`, `.exs`, `.hs`, `.ml`, `.fs`, `.fsx`, `.clj`, `.cljs`, `.cljc`, `.erl`, `.hrl`, `.r`, `.jl`, `.zig`, `.m`, `.groovy`, `.gvy`, `.gradle`, `.ps1`, `.psm1`, `.sql`, `.vb`, `.sh`, `.bash`, `.zsh` and `.ksh` files (`.h` is read as C++) returns a terse view built inside `zazzled` with tree-sitter (`src/terse.rs`), in the same line format and with no sha; `--source` returns the code. A file with nothing to shorten, or over 1 MiB, comes back as source with its sha.

- Hidden or collapsed: runs of 2+ lines of top-level imports (`1-6 ⋯ 3 import(s)`; Rust `use`, Go `import (..)`, Java and Kotlin `import`, Swift and Scala `import`, C and C++ `#include`, PHP `use`, Ruby `require`/`require_relative`, Perl `use`, Lua `local x = require ..`, Elixir `alias`/`import`/`require`/`use` at the top or inside a `defmodule`, Haskell `import`, OCaml and F# `open`, Clojure `(require ..)`/`(import ..)`/`(use ..)` and the `(:require ..)` clauses of `ns`, Erlang `-include`/`-include_lib`/`-import`, R `library`/`require`/`source`, Julia `using`/`import`, Zig `const x = @import(..)`, Objective-C `#import`/`@import`, Groovy `import`, PowerShell `Import-Module`/`using`/`. file`, Visual Basic `Imports`, shell `source`/`.`); logging statements on loggers (`log`, `logger`, `_logger`, `self.logger`, `this.logger?`, `LOG`, `slog`, ...), JavaScript and TypeScript `console.log/debug/info/warn/error/trace`, Dart `debugPrint`/`log`, Rust `debug!`/`info!`/`warn!`/`error!`/`trace!`/`event!` (bare or under `log::`/`tracing::`), Android `Log.d/i/w/e/v` and Timber, Swift `NSLog`/`os_log`/`debugPrint`, C and C++ `spdlog::`/`logger->` calls, `LOG_*`/`DLOG*`/`ALOG*`/`VLOG*`/`LOGE`-style macros `LOG(INFO) << ..`/`qDebug() << ..` streams, Ruby `logger.*`/`Rails.logger.*` and PHP `$this->logger->*`, `Log::*` and `error_log`, Perl `$log->debug(..)`-style calls and Log4perl's bare `DEBUG`/`INFO`/`WARN`/`ERROR`/`TRACE`, Lua `log.*`/`logger:*`, Elixir `Logger.*`, Haskell `logDebug`/`logInfo`/`$(logWarn)` (monad-logger) and `debugM`/`infoM` (hslogger), OCaml `Logs.*`/`Log.*`, Clojure `log/*` and `timbre/*`, Erlang `?LOG_*` macros and `logger:*`/`lager:*`, R `log_*` (logger) and `flog.*` (futile.logger), Julia `@debug`/`@info`/`@warn`/`@error`/`@logmsg`, Zig `std.log.*`/`log.*` (`std.debug.print` stays), Objective-C `NSLog`/`os_log`/`DDLog*`, PowerShell `Write-Verbose`/`Write-Debug`/`Write-Information`/`Write-Log` without `$(..)` or a pipe (`Write-Host` stays), Visual Basic `Debug.*`/`Trace.*`, shell commands named `log`, `log_*`, `debug`, `debug_*` or `logger` with no `$(..)`, process substitution or arithmetic in their arguments (`echo` and `printf` stay, they are the script's output), when they are alone on their lines and have no assignment, await, yield, increment, decrement, Go channel receive or Rust `?` in their arguments (Go `log.Fatal*` and `log.Panic*` stay: they stop the program); `if` blocks without `else` that check a log level (`isDebugEnabled`, `isEnabledFor`, ...) and only log; region markers (`# region`, `// #region`, `//#endregion`, `--#region`, `;; region`, `%% region`, `(* region *)`, Visual Basic `#Region`, `#pragma region`; Swift `// MARK:` comments stay, they are section titles); multi-line constructors that only store parameters (`__init__` with `self.x = name`, docstrings, `pass` and `super().__init__(..)`; TypeScript `this.x = name` and `super(..)`; Dart constructors without a body whose initializers are `field = name` or `super(..)`; Java constructors with `this.x = name` and `super(..)`/`this(..)`; Rust functions whose body is only `Self { a, b: name }`; Go functions that only `return &T{a: name}`; Swift `init` with only `self.x = name`; C++ constructors with an empty body and an initializer list of `field_(name)`; Ruby `initialize` with only `@x = name`; PHP `__construct` with only `$this->x = $name` or promoted parameters; Groovy as Java; Visual Basic `Sub New` with only `_x = name` or `Me.x = name`) as `12-17 ⋯ __init__(self, api, cache): only assigns fields`; and catches that only log and rethrow (bare `raise`, `throw e` of the caught name, `rethrow`, C++ `throw;`, Swift `throw error` or the bound name, Ruby bare `raise` in a `rescue`, PHP `throw $e`, a Scala catch with one `case e: T =>` that throws `e`, Groovy `throw e`, Objective-C `@throw;`, PowerShell `throw`, Visual Basic `Throw`), whole when the clause has its own lines, else as `} catch (e) { ⋯ logs and rethrows }`; Go's `if err != nil {` blocks that only log and return the error with plain values (`return nil, err`) stay on one line as `if err != nil { return nil, err }` (`{ ⋯ logs; return nil, err }` when they logged).
- Shortened: `public`/`private`/`protected` on TypeScript class members (constructor parameter properties keep theirs) and on Java declarations; Kotlin and Swift visibility modifiers; PHP `public`/`private`/`protected` on methods, properties and constants (promoted constructor parameters keep theirs); Scala `private`/`protected`; Groovy as Java; Visual Basic `Public`/`Private`/`Protected`/`Friend`; Rust `pub`/`pub(crate)` except on `pub use`.
- Shell heredoc bodies of 6+ lines collapse to one note; the `<<EOF` and `EOF` lines stay. SQL files are read line by line (`src/sql.rs`): `-- region` and `/* region */` comments go, and an `INSERT .. VALUES` whose 6+ rows each sit alone on their lines keeps its first and last rows and collapses the rest (`⋯ 5 row(s)`).
- In Haskell, OCaml, F#, Clojure, Erlang, R and Julia a block's last expression is its value, so a log call there stays; Clojure log calls are hidden only in body forms (`defn`, `let`, `do`, `when`, ...), not as arguments.
- Short locals (only in the languages listed in the heading up to Elixir and in Zig, Objective-C and Groovy; Groovy only in methods without closures, lambdas or command calls, which its grammar misreads; not in Haskell, OCaml, F#, Clojure, Erlang, R, Julia, PowerShell, SQL, Visual Basic or Nim, whose pattern bindings, shadowing, dynamic or case-insensitive names make renames unsafe without a compiler; not in shell scripts, where `local` variables are dynamically scoped and reached by name through `read`, `unset`, `printf -v`, `${!x}` and `declare -n`): locals of 6+ characters get the C# view's short names (`invoice_rows` → `ir`, `runningTotal` → `rt`, `response` → `res`; a clash, keyword or common builtin gets a number) at their declaration and every use (`src/locals.rs`). Without a compiler, a name is renamed at every place it appears in its function, and only when that is provably safe. It keeps its name when it is also a parameter, a nested function or class name, a JSX tag, an object or struct shorthand key, a Go struct-literal key, a Java label or annotation, in a Java local or anonymous class, in `global`/`nonlocal`, an import or a Rust `use`, a Rust macro name or a Rust format string (`{name}`), a Kotlin `$name` template, a C++ init-capture (`[x = y]`), a Ruby method called without a receiver or used before its first assignment, a PHP static property (`self::$x`), `global`/`static` declaration or variable variable (`$$x`), a Scala lambda parameter, a Perl sub's `@_`/`shift` parameters or a name written with another sigil (`$x` vs `$x[0]`, which is `@x`), an Elixir function-head argument or a name bound in a `case`/`fn` pattern, a Lua or Perl name used inside its own declaration (`local x = x + 1`, where it means the outer `x`) or, as in Ruby, before its first declaration, inside a preprocessor line or named in a `#define` of the same file, also written as a type name (C++'s vexing parse), inside a `match` case or a self-documenting f-string field (`{x=}`); outside Python, Ruby, PHP and Elixir (where a local belongs to the whole function) also when it is declared twice (Rust shadowing, Go `:=` redeclaration) or used outside the block that declares it; and in a function that uses `eval`, `locals()`, `vars()`, `exec`, `with`, Ruby `binding`/`local_variable_get`, PHP `compact`/`extract`/`get_defined_vars`, Perl `no strict` or a string `eval`, Lua `load`/`loadstring`/`setfenv`/`getfenv` or `debug.getlocal`/`setlocal` calls, or Elixir `quote`/`binding`/`var!`, whose parse has errors, or that a macro made the parser misread (a C "function" without a function declarator). Members (`obj.name`) and keyword or named-argument labels are other things and keep their text.
- Kept: everything else, including `print` and logging that shares a line with other code.
- Nim (`.nim`, `.nims`) has no tree-sitter grammar good enough, so `src/nim.rs` works line by line: runs of top-level `import`/`from .. import`/`include` lines (following brackets and trailing commas), indented one-line `debug`/`info`/`notice`/`warn`/`error`/`fatal`/`trace` calls (bare or on `log`/`logger`) with no `=`, `inc`, `dec`, `add`, `del` or `discard` and no continuation line, and `# region` comments; lines in `"""` strings are left alone.

## C# structure

`zazzle-csharp-indexer` lists no tools of its own: agents ask zazzle, and `zazzled` calls the indexer's daemon over the same bridge as the terse view (starting it when nothing listens), for the git work tree that holds the working directory. Answers come from the index the daemon keeps current, so they need no file reads. `root:path` in the indexer's answers becomes a path relative to the working directory.

| MCP tool | command | output |
|---|---|---|
| `zazzle_overview` | `overview` | file and line counts by language, top directories, entry points and project files, largest files; start here on an unfamiliar repository |
| `zazzle_find_symbol` | `find-symbol NAME [--exact] [--kind K] [--limit N] [--offset N]` | `path:line kind container.name \| signature`, exact matches first; C# parsed with Roslyn, other languages by a regex extractor |
| `zazzle_changed_since` | `changed-since [CURSOR]` | `A`/`M`/`D path` since a cursor from an earlier call, then a new cursor; cursors expire when the indexer's daemon restarts |
| `zazzle_git_log` | `git-log [PATH] [-n N] [--grep TEXT]` | `hash date author subject` |
| `zazzle_git_blame` | `git-blame PATH [--start N] [--end N]` | `git blame -w` for a range (default 40 lines) |
| `zazzle_report_gap` | `report-gap TEXT` | records what the agent needed and could not get in the indexer's usage log, where `zazzle-csharp-indexer report` lists it |

Where a symbol is used is `zazzle_refs` (`refs`); the indexer's `find_references` is gone. Unlike it, `refs` shows each use's enclosing symbol, but it lists declarations of fields, locals and parameters as uses rather than definitions, and pages with `-m` and narrower paths instead of an offset.

## Edits need the sha

Every edit except `--create` needs proof that the caller read the current code: `--expect SHA` from `read --source`, or `--at` anchors from `read --source --anchors`. A refusal never reveals the current sha or anchors (`the file is not the version you read; read it again with --source`). A successful edit returns the new sha, since the caller wrote the change.

## Symbols

`zazzle read FILE --symbol NAME` (`-s`) reads one symbol instead of the whole file: the outline gives its range, which is extended upward over the doc comments, comments and attributes directly above it (`///`, `/** */`, `#[...]`, C# `[...]`, `@decorator`, Python `#`), so a C# method's XML doc comes with it.

- `NAME` is the symbol's name or the last parts of its qualified name: `Invoice.Total`, `billing::tax::total`. Containers come from nesting (classes, impls, modules, namespaces); `impl Display for A` counts as `A`; generics are ignored. Exact case is tried first, then any case.
- Overloads are picked by parameter types: `Total(decimal, int)`, `Total()`. Types are compared as written in the declaration, ignoring spaces (`Dictionary<string,int>` matches `Dictionary<string, int>`, but `decimal` does not match `System.Decimal`); both `Type name` and `name: Type` declarations work, defaults, attributes and modifiers (`ref`, `out`, `params`, `mut`) are ignored, receivers (`self`, `&self`, Python's `self`/`cls`) are not counted, and an untyped parameter matches by name. A list that fits no overload shows the overloads there are.
- Several matches (overloads, the same method in two impls) are listed with their lines and declaration line, numbered; add the parameter types or `#N` (`Total#2`) to pick one. No match lists similar names, or points at `zazzle outline`.
- The usual budget applies, and `--symbol` cannot be combined with `PATH:A-B`, `--around`, `--head` or `--tail`. Over MCP, `zazzle_read` takes `symbol`.

## Clean

`grep -rn foo src | zazzle clean` (or `cargo test 2>&1 | zazzle clean`) reads standard input, up to 64 MiB, and prints what an agent needs from it:

| step | what it does | `--lossless` |
|---|---|---|
| ansi | removes colours, cursor moves and window titles | yes |
| whitespace | keeps the last frame of `\r` redraws, trims line ends, squeezes blank runs | yes |
| json | a JSON document or JSON Lines loses whitespace outside strings; keys and numbers stay as written | yes |
| grep | `path:N:text` / `path-N-text` / `path:text` output (grep, git grep, rg) is regrouped by file in `find`'s layout, only if every line parses and it is shorter | yes |
| repeats | runs of identical lines become one line and `… previous line repeated N more times` | yes |
| progress | a run of progress-bar lines becomes its last line and `… N progress lines dropped` | no |
| blobs | base64 or hex runs of 200+ characters become `<blob N chars>` | no |
| long-lines | lines over 600 bytes keep 300 bytes and `[line was N bytes]` (not for JSON) | no |
| budget | above `--budget` (default 8000 tokens) the head and tail stay and the middle becomes one line saying how much was cut | always |

Every removal leaves a marker. Input that is not recognised only gets the lossless steps, and if the markers would make the text longer, the input is passed through. `--as` skips detection (`text` turns off the json and grep steps). `--stats` adds `[clean ~A→~B tokens; steps: …]`. Repeating the same cleaned output in a session gets the usual `already sent` notice. `clean` is not an MCP tool: an MCP client already holds the text. To clean what another MCP server returns, use the [MCP proxy](#mcp-proxy).

## Run

`zazzle run -- cargo test` runs a command in the caller's environment and prints what an agent needs from its output. The invoker `exec`s `zazzled wrap ARGS` (on Intel Macs run `zazzled wrap` directly); it never runs inside the daemon ([why](src/.dev/decisions.json)).

- stdout and stderr go through one pipe, so their order is kept; stdin is the caller's. The exit status is the command's (128 + N for a signal); `--timeout SECS` (default 3600) kills it and exits 124. Up to 64 MiB of output is read.
- A profile, picked from the command (through `npx`, `uv run`, `python -m`, `env VAR=x`, …) or named with `--profile`, drops only lines it knows are noise and counts them. Lines it does not recognise are always kept.

| profile | commands | drops |
|---|---|---|
| cargo | `cargo …`, `cargo-nextest` | progress (`Compiling`, `Running`, `Downloaded`, …), `test … ok`, nextest `PASS`, empty test runs, std/core frames in backtraces |
| pytest | `pytest`, `python -m pytest` | session header, `PASSED` lines, progress lines with no failure |
| go | `go test`, `go vet`, `go build` | `ok` packages, `--- PASS`, `=== RUN`, packages without tests |
| dotnet | `dotnet …` | restore and `->` build lines, banners, `Passed` tests, System/Microsoft/xUnit/NUnit frames, the repeated warnings and errors MSBuild prints again at the end |
| js-test | `jest`, `vitest`, `mocha`, `npm test` and the like | `PASS` files, `✓` tests, banners |
| tsc | `tsc`, `vue-tsc` (also through `npx`) | `tsc -b --verbose` progress and the `--pretty` per-file summary table; diagnostics are grouped under their file (`src/a.ts` then `1:14 error TS2322: …`), code frames kept |
| install | `npm/pnpm/yarn/bun install`, `pip install`, `uv pip install` | download and resolve progress |

- Then `clean`'s steps and budget apply (`--lossless` turns off the profile and the lossy steps), and a footer says what happened: `[exit 101 in 8.4s; cargo profile dropped 6 progress lines, 333 passing tests]`.
- Measured: this crate's `cargo test` (334 passing tests) goes from about 5,300 tokens to about 80; 40 `tsc` errors across 4 files go from about 1,300 tokens to 850 (2,800 to 1,500 with `--pretty`).

## MCP proxy

`zazzle proxy` sits between an MCP client and another MCP server and runs `clean`'s steps over the text the server sends back:

```bash
claude mcp add fs -- zazzle proxy -- npx -y @modelcontextprotocol/server-filesystem ~/src      # stdio server
zazzle proxy --upstream https://mcp.example.com/mcp --listen 127.0.0.1:8808                      # Streamable HTTP server
claude mcp add --transport http example http://127.0.0.1:8808/mcp
```

- `zazzle proxy ARGS` replaces itself (`exec`) with `zazzled proxy ARGS` and does not run inside the daemon ([why](src/.dev/decisions.json)). On Intel Macs, where there is no `zazzle`, run `zazzled proxy` directly.
- What changes: the `text` of `content` items (and of embedded `resource` items) in `tools/call` results, and the `text` of `contents` in `resources/read` results. A text is replaced only when that saves at least one token.
- What never changes: `structuredContent`, images and other binary items, errors, protocol messages (`initialize`, lists, notifications), requests in either direction, and replies to anything else. Responses are matched to requests by JSON-RPC id, single or batched.
- Options: `--lossless` and `--budget T` as for `clean` (budget per text, default 8000); `--skip TOOL` (repeatable) leaves one tool's results alone; `--log` writes `tools/call NAME ~A→~B tokens` to standard error for each cleaned result; `--config FILE` sets them per tool.
- `--config FILE` is JSON. Each tool (by exact name, else the longest matching `*` pattern) and resource reads can set `lossless`, `budget` and `skip`; what a rule leaves out comes from the command line. Unknown sections, settings or types stop the proxy at startup.

  ```json
  {"tools": {"read_file": {"lossless": true, "budget": 30000}, "github_*": {"budget": 2000}, "get_secret": {"skip": true}},
   "resources": {"lossless": true}}
  ```
- stdio: the server runs as a child with stderr passed through; the proxy exits with its exit code when it closes its output.
- HTTP: only Streamable HTTP (MCP 2025-03-26 and later), not the older HTTP+SSE transport. Connections stay open for further requests unless the client asks to close them (`Connection: close`, or HTTP/1.0 without keep-alive), sends something unreadable or stays idle for 60 seconds; replies of unknown length are chunked on open connections, and the upstream side reuses its connections too. JSON replies are cleaned whole (up to 64 MiB, larger ones are passed through), `text/event-stream` replies event by event as they stream, and anything else (including compressed bodies) is copied. Headers such as `Mcp-Session-Id` and `Authorization` pass both ways; hop-by-hop headers are dropped, bodies are requested uncompressed, redirects and error statuses are returned to the client. `Origin` headers from hosts other than localhost are refused (403). It listens on `127.0.0.1:8808` unless told otherwise; request bodies need `Content-Length`. Upstream TLS uses the operating system's trust store, and `HTTPS_PROXY`/`NO_PROXY` are honoured.

## Stored outputs

`clean`, `run` and `proxy` outputs cannot be produced again (their input was a pipe, a command's output or another server's reply), so they are stored on disk under an id: `clean` and `run` replies start with `[r:ID]`, and the proxy adds an item `[r:ID shortened by zazzle]` after each text it shortened (its own item, so JSON text stays JSON). When something was cut (a budget, a profile, progress bars, blobs, long lines), the reply says `read-again r:ID --full`.

- `zazzle read-again r:ID` returns the output exactly as it was shown; `--full` returns the uncut text with its line numbers (terminal escapes removed), with `--lines A-B` and `--budget T` (default 3000).
- The store is `~/.zazzle/outputs` (`$ZAZZLE_OUTPUTS_DIR`), mode 0700 with 0600 files. Each save reaps entries older than an hour; if the store then holds more than `$ZAZZLE_OUTPUTS_MAX_GB` gigabytes (default 2), the oldest go until half the bytes remain. A failed save keeps the output and says it was not stored.

## zazz scripts

Agents (and humans) can ship small programs to zazzled: a `.zazz` file is a ZIP (stored or deflate, read with built-in code) holding `zazz.json`, Rust sources (`src/`, `tests/`) or a prebuilt WASM module. Design and decisions: `ZAZZ-PLAN.md`; agent how-to: `agents/skills/zazz/SKILL.md`.

| command | does |
|---|---|
| `zazz new NAME` | a script directory to start from (manifest, `src/main.rs`, `tests/basic.rs`) |
| `zazz test DIR\|FILE [--update]` | builds to WASM and runs the tests; replies with diagnostics or a test report; registers nothing |
| `zazz pack DIR [-o FILE]` | a deterministic `.zazz` (same sources, same bytes, same id) |
| `zazz add FILE` | reads the file once, ids it by the BLAKE3 of its exact bytes, builds and tests once, registers; `cached` when known |
| `zazz call ID [JSON\|-]` / `zazz call ID OP [--params J] [--body J]` | runs `json-command` or one OpenAPI operation in a warm sandboxed worker; JSON in, JSON out |
| `zazz describe\|ls\|rm` | what a script is and may touch; the registry |
| `zazz install FILE\|ID [--as NAME]`, `zazz run NAME ARGS`, `uninstall`, `list` | commands for humans: `~/.zazzle/bin/NAME` links to the invoker; flags come from the manifest's `cli` spec |
| `zazz keygen NAME [--native] [--network ...]`, `zazz sign FILE`, `zazz trust add\|ls\|rm` | signing authorities and the machine's trust in them |
| `zazz doctor` | rustc, the `wasm32-wasip1` target, zazz-worker, the OS sandbox, paths |

- **Runtime:** Rust scripts compile (std + the embedded `zazz` SDK, built once per toolchain) to `wasm32-wasip1` and run in `zazz-worker` (wasmtime). Each call gets a fresh instance with time and memory limits; workers are kept warm per (script, directory, policy) and recycled.
- **Two sandbox layers:**
  - WASI preopens only the manifest's paths; the `zazz_host` imports carry the call, progress, cancellation and allowlisted HTTPS.
  - The OS sandbox wraps every worker, `rustc` build and test run: Landlock + seccomp on Linux, `sandbox-exec` on macOS. Unsigned scripts never run without it.
- **Signing:** a `zazz.sig` (ECDSA P-256 over a canonical digest, plus the signer's chain) is checked at add. Authorities are certificates in root-owned `/etc/zazzle/trust.d` (and the Keychain on macOS); their grants (network hosts, paths, native) narrow what the script may do. Verdicts are cached for `trust_ttl` (root-owned `/etc/zazzle/policy.json`, default 600 s) and refreshed in the background, so a call never waits on trust. Only signed and trusted scripts get network; scripts trusted for native code also get a host build and run natively under the OS sandbox, falling back to WASM when trust is withdrawn.
- **Streams:** through the invoker a script reads and writes the caller's real stdin, stdout and stderr; over MCP the output is captured into the reply.
- **Store:** `~/.zazzle/v1/zazz/` (0700) keeps compiled modules, native binaries and metadata, never the `.zazz` itself.

## Session ids and dedupe

With `ZAZZLE_SESSION` set (or over MCP, where each connection is a session) every response starts with `[r:ID]`. Repeating a request whose response is identical and was sent within the last 40,000 estimated tokens returns `[r:ID] already sent: zazzle read a.rs:1-20, unchanged. If you no longer have it: read-again r:ID`.

`zazzle read-again r:ID` (MCP `zazzle_read_again`) runs the request behind a session response again, in its original working directory: the same text under the same id when nothing changed, else the current version under a new id with `r:OLD changed since it was sent`. So an agent can drop a response from its context (or lose it to compaction) and get it back exactly. The session keeps the last 512 requests; `clean`, `run` and `proxy` outputs come from the output store instead (see [Stored outputs](#stored-outputs)). Responses read again never reveal more than the original request did: a terse view stays without a sha. Edits are never deduplicated. Without a session there are no ids and no dedupe. State lives in daemon memory only.

## Cache

The daemon keeps derived data only, never file text, in `~/.zazzle/v1/` (`ZAZZLE_CACHE_DIR`); design notes: [src/.dev/decisions.json](src/.dev/decisions.json), file format: [src/.dev/contracts.json](src/.dev/contracts.json).

## Measurements

Linux x86_64 VM, p50 per process spawn over 3000 runs, microseconds (`macos/c/zazzle/bench/run.sh`): floor (`_exit`) 120, `zazzle version` 209, `zazzle read` 228 vs `cat` 1057, `zazzle find` on a small tree 554 vs `grep -rn` 1278, libc hello 941. On a 111 MB tree of 6,751 files: `find` 25-35 ms vs `grep -rn` 150 ms; `ls` with line counts 1088 ms cold, 52 ms after a daemon restart; walk share of `find` fell from 33 ms to 10 ms with the validated listing cache (the decision this supports: [src/.dev/decisions.json](src/.dev/decisions.json)). macOS (GitHub Actions virtual M1, noisy, 3000 runs, p50): floor 1734, libc hello 1662, `zazzle version` 1799, `zazzle read` 1736 vs `cat` 1938, `zazzle find` on a small tree 2361 vs `grep -rn` 2471, so `zazzle` costs about what any libSystem program costs and the gain there is in output size, not startup. On macOS the floor is dyld plus libSystem ([why](../../c/zazzle/.dev/decisions.json)), so the invoker can match but not beat a libSystem hello; the MCP front end avoids the spawn entirely.

## Safety

The daemon answers only the same uid (`SO_PEERCRED` / `getpeereid`; the invoker checks the daemon too), the socket is mode 0600, requests are size-limited, and edits refuse paths outside the caller's working directory after resolving symlinks. Reads are allowed anywhere the user can read. Hard-linked files are rewritten in place; extended attributes are not preserved by the atomic replace.

## Limits

Regex is the Rust dialect (no look-around, no backreferences). `zazzle` itself supports macOS arm64 and Linux x86_64/aarch64; on Intel Macs use `zazzled mcp` or `zazzled run`. Outlines are heuristic for languages without a grammar and for files over 1 MiB (tree-sitter would take about 1.5 s on an 8.7 MB generated parser). How parallel work is run (one thread pool per daemon) and why: [src/.dev/decisions.json](src/.dev/decisions.json). `agent-fs` (`afs`) is a separate tool in this repository with overlapping goals.

## Tests

`cargo test -p zazzled`; branch coverage (nightly, test code excluded, target 95%): `sh zazzled/coverage.sh 95` from `macos/rust` (currently 100%). C invoker: `make unit coverage smoke` in `macos/c/zazzle` (branch coverage of the invoker logic, 100%).

Why each dependency: [.dev/decisions.json](.dev/decisions.json).
