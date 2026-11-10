---
name: todo-and-worklog
description: Maintain a personal TODO.md and a per-meeting WORKLOG.md with a one-minute summary. Use for daily planning or preparing for a team meeting.
---

# todo-and-worklog

## TODO.md
- Sections: Today, Next, Later, Done (cleared next day). Today and Next hold only what can be done now; blocked items go to Later with the blocker.
- Holds small items not in the team tracker.

## WORKLOG.md
- Grouped by meeting date. Each entry: "In one minute" (3-5 plain bullets, product first, readable by non-developers), then Today and Details.
- Add merged PRs: `gh pr list --author @me --state merged --search "merged:>=<date>"`.

## Rules
- Keep files in the user's chosen folder; ask once if unknown.
- Edit in place; never delete the user's entries without asking.
