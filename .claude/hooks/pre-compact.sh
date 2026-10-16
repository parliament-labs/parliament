#!/usr/bin/env bash
# PreCompact hook — writes a short state snapshot to memory/ so a
# post-compaction session can see where work was left off (branch,
# dirty files). Overwritten on every compact — ephemeral by nature.
set -euo pipefail

proj="${CLAUDE_PROJECT_DIR:-$(pwd)}"
snapshot="$proj/memory/pre_compact_snapshot.md"

branch=$(git -C "$proj" branch --show-current 2>/dev/null || echo "(detached)")
status_body=$(git -C "$proj" status --porcelain 2>/dev/null | head -40 || true)
[[ -z "$status_body" ]] && status_body="(clean)"
ts=$(date -u '+%Y-%m-%dT%H:%M:%SZ')

{
  echo "---"
  echo "name: Pre-compact snapshot"
  echo "description: Transient branch + dirty-tree state saved just before context compaction. Overwritten on every compact."
  echo "type: project"
  echo "---"
  echo ""
  echo "Snapshot taken at $ts."
  echo ""
  echo "- **Branch:** $branch"
  echo ""
  echo "**git status --porcelain (first 40 lines):**"
  echo ""
  echo '```'
  echo "$status_body"
  echo '```'
} > "$snapshot"
