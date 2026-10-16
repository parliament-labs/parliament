#!/usr/bin/env bash
# rebuild.sh — pull latest main, rebuild release WASM + noctua, restart.
#
# Usage:
#   ./rebuild.sh             # full cycle: pull, build, restart noctua
#   ./rebuild.sh --no-pull   # skip git pull, build the current working tree
#   ./rebuild.sh --no-run    # build only; don't kill / restart noctua
#   ./rebuild.sh --help      # this help

set -euo pipefail

PULL=1
RUN=1
for arg in "$@"; do
    case "$arg" in
        --no-pull) PULL=0 ;;
        --no-run)  RUN=0  ;;
        --help|-h) sed -n '2,8p' "$0" | sed 's/^# //'; exit 0 ;;
        *) echo "rebuild.sh: unknown flag: $arg" >&2; exit 2 ;;
    esac
done

# Always run from the repo root — trunk reads ./Trunk.toml, noctua serves
# ./dist relative to its CWD.
cd "$(dirname "$0")"

if [[ $PULL -eq 1 ]]; then
    echo "==> git pull --ff-only"
    git pull --ff-only
fi

echo "==> trunk build --release"
trunk build --release

echo "==> cargo build --release -p noctua"
cargo build --release -p noctua

if [[ $RUN -eq 1 ]]; then
    # Stop any noctua already bound to :3000. lsof exits non-zero when no
    # PID matches, so guard the kill instead of triggering `set -e`.
    if pids="$(lsof -ti tcp:3000 2>/dev/null)" && [[ -n "$pids" ]]; then
        echo "==> stopping existing noctua (PID $pids)"
        kill $pids
        for _ in 1 2 3 4 5; do
            sleep 1
            if ! lsof -ti tcp:3000 >/dev/null 2>&1; then break; fi
        done
    fi
    echo "==> starting noctua — http://localhost:3000 (Cmd+Shift+R in the browser to dump cached WASM)"
    exec ./target/release/noctua
fi

echo "==> done — re-run without --no-run to start noctua"
