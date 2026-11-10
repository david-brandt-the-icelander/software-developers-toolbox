---
name: large-file-writes
description: Write files over ~150 lines without hitting the stream idle timeout. Use when creating or heavily rewriting a long file.
---

# large-file-writes

Long content in one tool call or in visible response text can fail with `Stream idle timeout`.

## Steps
1. `Write` a skeleton under ~30 lines: headings plus one placeholder per section.
2. `Edit` once per section, replacing its placeholder; keep each payload under ~150 lines.
3. Never print the full file as response text first.
