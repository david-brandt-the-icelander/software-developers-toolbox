---
name: machine-load
description: Keep a Mac responsive during heavy builds, tests, scans or copies, and diagnose slowness or swap pressure. Use before large local runs or when the user says the machine is slow.
---

# machine-load

First ask whether CI can run the build instead; prefer it.

## Modes
- Interactive: user active in the last 10 minutes, or 07:00-22:00. Run heavy commands niced: `taskpolicy -b <cmd>` (lowers CPU and disk priority).
- Unattended: night and idle, or the user said so. Run wide, sized by memory (peak footprint times width must fit in RAM).

## Diagnose
- Swap: `sysctl vm.swapusage`; footprints: `top -o mem -l 1 | head -20`. Do not trust `memory_pressure -Q`.
- Kill or stop your own largest process first; priority cannot fix swap.
- Run whole-solution tools (`dotnet format`, workspace analyzers) per project; they can use tens of GB.
- New multi-GB trees trigger Spotlight: add `.metadata_never_index` to the root; get reference copies with `git worktree add`, `git show` or `git archive`, never by copying a checkout.
- Check other running sessions before stacking more work.

## Limits
- Keep at least 20 GB disk free.
- Tool timeouts mean the width is too high; reduce it.
- Report the mode used and anything killed.
