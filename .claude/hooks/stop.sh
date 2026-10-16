#!/usr/bin/env bash
# Stop hook — fires a cmux notification with a short diff summary so you
# get a visual ring when a long task wraps. Also signals cmux's canonical
# lifecycle hook endpoint.
set -euo pipefail

proj="${CLAUDE_PROJECT_DIR:-$(pwd)}"

summary=$(git -C "$proj" diff --stat 2>/dev/null | tail -1 | xargs || true)
[[ -z "$summary" ]] && summary="(no changes)"

branch=$(git -C "$proj" branch --show-current 2>/dev/null || echo "")
subtitle="$branch"

if command -v cmux >/dev/null 2>&1; then
  cmux notify --title "Claude done" --subtitle "$subtitle" --body "$summary" 2>/dev/null || true
  cmux claude-hook stop 2>/dev/null || true
fi
