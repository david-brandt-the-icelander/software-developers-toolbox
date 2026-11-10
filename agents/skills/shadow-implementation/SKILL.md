---
name: shadow-implementation
description: Hand a well-defined coding spec to a second agent in its own worktree while you write your own version, then compare. Use for a small, clearly specified change where a second opinion is cheap.
---

# shadow-implementation

1. Write the spec: goal, constraints, files, tests to satisfy.
2. Start the shadow agent (`claude -p "<spec>"`) in a separate worktree (`git worktree add ../shadow-<name>`), in the background.
3. Do your own implementation meanwhile.
4. When it finishes, run its tests yourself in its worktree before reading its summary.
5. Compare diffs: correctness, edge cases, simplicity. Take the better one or merge the best parts; record the verdict in one line.
6. Remove the shadow worktree.
