---
name: recover-failed-node
description: Diagnose and recover a dev, staging or production node that will not start because of a startup or migration failure. Use when a service is down and its normal admin tools are unavailable.
---

# recover-failed-node

1. Read the startup failure (logs, exception) before touching anything.
2. Query the environment database directly; tools that depend on the app are down. Use the credentials path the environment documents, never paste secrets.
3. Emit the migration script the app would run; know what it does before running it, comparing against the applied-migrations table.
4. Apply with one connection for the whole run, splitting only on lines that are just `GO`; guards like `SET NOEXEC ON` are per connection.
5. Restart via a path that needs no database (ops restart command or orchestrator); note that auto-restart may get there first.
6. Verify the node is healthy and say exactly what you verified.
7. Production changes need the user's explicit yes; afterwards point out how the deploy pipeline should have prevented it.
