#!/usr/bin/env bash
# PreToolUse(Bash) hook — blocks a small set of unambiguously destructive
# patterns that should never run without explicit user confirmation.
# Exit 2 + stderr message blocks the tool call; exit 0 allows.
# Stay conservative: false positives here turn into friction, so match
# only patterns with clear intent, not guesses.

set -euo pipefail

input=$(cat)
cmd=""
if command -v jq >/dev/null 2>&1; then
  cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null || true)
fi
[[ -z "$cmd" ]] && exit 0

deny() {
  printf 'BLOCKED by pre-tool-use hook: %s\nCommand: %s\n' "$1" "$cmd" >&2
  exit 2
}

# Filesystem destruction: rm -rf family.
case "$cmd" in
  *"rm -rf"*|*"rm -fr"*|*"rm -Rf"*|*"rm -fR"*|*"rm -rfi"*|*"rm -rfd"*)
    deny "rm -rf pattern — confirm with user before running." ;;
esac

# SQL destruction on whole databases/schemas. Case-insensitive match
# via grep -i since SQL is case-insensitive.
if printf '%s' "$cmd" | grep -qiE '\b(DROP[[:space:]]+(DATABASE|SCHEMA)|TRUNCATE[[:space:]]+TABLE)\b'; then
  deny "destructive SQL (DROP DATABASE / DROP SCHEMA / TRUNCATE TABLE) — confirm first."
fi

# Force-push to main/master on origin.
if printf '%s' "$cmd" | grep -qE 'git[[:space:]]+push([[:space:]]+[^|;&]*)?[[:space:]]-{1,2}f(orce)?\b([^|;&]*[[:space:]]origin[[:space:]]+(main|master)\b)'; then
  deny "force-push to main/master — confirm before overwriting shared history."
fi
if printf '%s' "$cmd" | grep -qE 'git[[:space:]]+push([[:space:]]+[^|;&]*)?[[:space:]]+origin[[:space:]]+(main|master)\b[^|;&]*-{1,2}f(orce)?\b'; then
  deny "force-push to main/master — confirm before overwriting shared history."
fi

# Git history rewrites against main.
if printf '%s' "$cmd" | grep -qE 'git[[:space:]]+reset[[:space:]]+--hard[[:space:]]+(origin/)?(main|master)\b'; then
  deny "git reset --hard main/master — irrecoverable if there's uncommitted work."
fi

exit 0
