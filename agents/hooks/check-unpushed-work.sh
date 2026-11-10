#!/bin/bash
# Stop hook that blocks once per state while commits, changes or stashes exist only on this machine.
p=$(cat); cd "$(printf '%s' "$p" | jq -r '.cwd // "."')" 2>/dev/null
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0
b=$(git rev-parse --abbrev-ref HEAD); [ "$b" = HEAD ] && exit 0
dirty=$(git status --porcelain); stash=$(git stash list | wc -l | tr -d ' ')
if up=$(git rev-parse --abbrev-ref '@{u}' 2>/dev/null); then un=$(git log --oneline "$up..HEAD"); else un=$(git log --oneline -20); note="no upstream; never pushed"; fi
[ -z "$dirty$un" ] && [ "$stash" = 0 ] && exit 0
state=$(printf '%s|%s|%s|%s' "$b" "$dirty" "$un" "$stash" | shasum | cut -d' ' -f1)
mark="${TMPDIR:-/tmp}/claude-stophook-$(printf '%s' "$p" | jq -r '.session_id // "x"')"
[ "$(cat "$mark" 2>/dev/null)" = "$state" ] && exit 0
printf '%s' "$state" > "$mark"
jq -nc --arg r "At-risk work on $b ${note:+($note)}: unpushed commits:
$un
uncommitted:
$dirty
stashes: $stash
Commit, push and open a PR now. If impossible, end with a line naming what would be lost." '{decision:"block",reason:$r}'
