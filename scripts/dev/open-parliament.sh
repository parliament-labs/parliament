#!/usr/bin/env bash
# open-parliament.sh — spawn a cmux workspace with the full Parliament
# dev loop: Noctua backend (top), trunk serve (right split), browser
# pane pinned to the Tyto URL.
#
# Idempotency note: always creates a NEW workspace. If you want to
# reuse, focus the existing one in cmux and skip this script.
#
# Usage:
#   scripts/dev/open-parliament.sh           # default layout
#   PARLIAMENT_TYTO_URL=http://localhost:9000 scripts/dev/open-parliament.sh
#
# Requires:
#   - cmux on PATH (manaflow-ai/cmux)
#   - cargo, trunk installed (the backend + frontend commands will fail
#     gracefully in their own surfaces if they're missing)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="${PARLIAMENT_REPO:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TYTO_URL="${PARLIAMENT_TYTO_URL:-http://localhost:8080}"

if ! command -v cmux >/dev/null 2>&1; then
  echo "cmux not found on PATH. This script requires manaflow-ai/cmux." >&2
  echo "  Install from https://cmux.com/ or run the commands manually:" >&2
  echo "    (terminal 1)  cd $REPO && cargo run -p noctua" >&2
  echo "    (terminal 2)  cd $REPO && trunk serve" >&2
  echo "    (browser)     open $TYTO_URL" >&2
  exit 1
fi

# Extract a ref (e.g. "workspace:N", "surface:M") from cmux command output.
ref_of() {
  local kind="$1" line="$2"
  printf '%s' "$line" | grep -oE "${kind}:[0-9]+" | head -1
}

echo "Spawning 'parliament-dev' workspace in $REPO..."
ws_out=$(cmux new-workspace --cwd "$REPO" --name "parliament-dev" --command "cargo run -p noctua")
ws_ref=$(ref_of workspace "$ws_out")
if [[ -z "$ws_ref" ]]; then
  echo "Failed to parse workspace ref from cmux output:" >&2
  echo "  $ws_out" >&2
  exit 1
fi
echo "  workspace: $ws_ref (backend starting)"

# Brief pause so the new workspace is fully registered before we manipulate it.
sleep 1

echo "Splitting right for 'trunk serve'..."
split_out=$(cmux new-split right --workspace "$ws_ref")
split_surface=$(ref_of surface "$split_out")
if [[ -z "$split_surface" ]]; then
  echo "Warning: could not determine split surface; trunk serve will need to be started manually." >&2
else
  sleep 0.3
  cmux send --workspace "$ws_ref" --surface "$split_surface" "trunk serve"
  cmux send-key --workspace "$ws_ref" --surface "$split_surface" Enter
fi

echo "Adding browser pane at $TYTO_URL..."
cmux new-pane --type browser --url "$TYTO_URL" --workspace "$ws_ref" >/dev/null

echo "Ready. Focus workspace: $ws_ref"
