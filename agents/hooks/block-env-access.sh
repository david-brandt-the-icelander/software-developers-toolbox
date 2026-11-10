#!/bin/bash
# PreToolUse hook that denies reading untracked .env files through Bash, Read, Grep or Glob.
in=$(cat)
case "$in" in *.env*) ;; *) exit 0 ;; esac
cd "$(printf '%s' "$in" | jq -r '.cwd // "."')" 2>/dev/null
# Prints candidate paths for the tool call, taking only readers' arguments for Bash.
candidates() {
  case "$(printf '%s' "$in" | jq -r '.tool_name')" in
    Bash) printf '%s' "$in" | jq -r '.tool_input.command // empty' | tr ';|&()' '\n' | grep -E '(^|[[:space:]/])(cat|head|tail|less|more|sed|awk|grep|rg|source|cp|mv|base64|strings|python3?|node)[[:space:]]' | tr -s ' ' '\n' ;;
    Read) printf '%s' "$in" | jq -r '.tool_input.file_path // empty' ;;
    Grep|Glob) printf '%s' "$in" | jq -r '.tool_input.path // empty' ;;
  esac
}
candidates | while IFS= read -r p; do
  case "${p##*/}" in .env|.env.*) ;; *) continue ;; esac
  [ -f "$p" ] || continue
  git ls-files --error-unmatch -- "$p" >/dev/null 2>&1 && continue
  jq -n --arg r "$p is an untracked .env file and may hold secrets; read .env.example or ask the user." '{hookSpecificOutput:{hookEventName:"PreToolUse",permissionDecision:"deny",permissionDecisionReason:$r}}'
  exit 0
done
exit 0
