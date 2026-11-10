---
name: why-is-it-like-this
description: Recover why code is the way it is from the written record, not a guess. Use when asked why something was done or who decided it.
---

# why-is-it-like-this

1. Repo first: comments, docs, ADRs, tests, change notes.
2. Pin the exact lines, then `git log -L`/`git blame -w`; skip formatting-only and generated-code commits (`git blame --ignore-rev`).
3. Commit to PR (`gh pr list --search <sha> --state merged`), then the review thread.
4. Ticket tracker for the decision, then chat search for decisions that never reached a ticket; check error monitoring only when the code looks like a fix.
5. Answer with sources, say what is evidence and what is inference, and say when the record is silent.
6. Stop when the cost of searching exceeds the value of the answer.
