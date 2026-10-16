#!/usr/bin/env bash
# SessionStart hook — one-line orientation on branch + dirty state.
# Also pings cmux so the workspace's sidebar reflects an active session.
set -euo pipefail

proj="${CLAUDE_PROJECT_DIR:-$(pwd)}"
branch=$(git -C "$proj" branch --show-current 2>/dev/null || echo "(detached)")
dirty=$(git -C "$proj" status --porcelain 2>/dev/null | wc -l | tr -d ' ')

printf 'Parliament session start — branch: %s (%s uncommitted)\n' "$branch" "$dirty"

if command -v cmux >/dev/null 2>&1; then
  cmux claude-hook session-start 2>/dev/null || true
fi
