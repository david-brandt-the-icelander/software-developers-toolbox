#!/bin/bash
# PreToolUse Bash hook that asks before any git push and denies PR merges.
cmd=$(jq -r '.tool_input.command // empty')
# Emits the hook decision JSON for the given decision and reason.
decide() { jq -n --arg d "$1" --arg r "$2" '{hookSpecificOutput:{hookEventName:"PreToolUse",permissionDecision:$d,permissionDecisionReason:$r}}'; }
# Removes heredoc bodies so quoted text is never mistaken for a command.
strip_heredocs() { awk 'd!=""{if($0==d)d="";next} match($0,/<<-?[ ]*["'"'"']?[A-Za-z_][A-Za-z0-9_]*/){d=substr($0,RSTART,RLENGTH);gsub(/^<<-?[ ]*["'"'"']?/,"",d)} {print}'; }
cmd=$(printf '%s' "$cmd" | strip_heredocs)
if printf '%s' "$cmd" | grep -qE 'gh[[:space:]]+pr[[:space:]]+merge([[:space:]]|$)|gh[[:space:]]+api\b.*pulls/[0-9]+/merge'; then
  decide deny "Merging a PR is the user's call; hand over the reviewed branch."
elif printf '%s' "$cmd" | grep -qE '(^|[;&|(])[[:space:]]*(sudo[[:space:]]+)?git([[:space:]]+-[^[:space:]]+([[:space:]]+[^-[:space:]][^[:space:]]*)?)*[[:space:]]+push([[:space:]]|$)'; then
  decide ask "Pushing publishes the branch. Confirm this push."
fi
exit 0
