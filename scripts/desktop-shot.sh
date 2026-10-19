#!/usr/bin/env bash
# Workspace-isolated screenshot helper for Ytrium visual tests.
#
# Implements the state-aware protocol:
#   * record the user's active workspace/monitor and whether special:app-test
#     is currently visible
#   * reveal the special workspace only if hidden
#   * capture with grim
#   * always restore the user's desktop (trap on EXIT/INT/TERM)
#
# Usage: desktop-shot.sh <output.png> [label]
set -uo pipefail

OUT="${1:?usage: desktop-shot.sh <output.png> [label]}"
LABEL="${2:-shot}"
SPECIAL="special:app-test"

mkdir -p "$(dirname "$OUT")"

# --- save state -------------------------------------------------------------
read -r SAVED_WS SAVED_MON <<<"$(hyprctl -j activeworkspace | python3 -c '
import json,sys
a=json.load(sys.stdin)
print(a["id"], a["monitorID"])
')"
SPECIAL_WAS_VISIBLE=$(hyprctl -j monitors | python3 -c '
import json,sys
print("yes" if any(m.get("specialWorkspace",{}).get("name") for m in json.load(sys.stdin)) else "no")
')
echo "[shot:$LABEL] active ws=$SAVED_WS monitor=$SAVED_M special_visible=$SPECIAL_WAS_VISIBLE"

restore() {
  local rc=$?
  # Only hide the special workspace if we were the ones to reveal it.
  if [[ "$SPECIAL_WAS_VISIBLE" == "no" ]]; then
    local still_visible
    still_visible=$(hyprctl -j monitors | python3 -c '
import json,sys
print("yes" if any(m.get("specialWorkspace",{}).get("name") for m in json.load(sys.stdin)) else "no")
')
    if [[ "$still_visible" == "yes" ]]; then
      hyprctl dispatch togglespecialworkspace app-test >/dev/null 2>&1 || true
    fi
  fi
  # Return the user to the workspace they were on, on the same monitor.
  if [[ "$SAVED_MON" != "0" ]]; then
    hyprctl dispatch focusmonitor "$SAVED_MON" >/dev/null 2>&1 || true
  fi
  hyprctl dispatch workspace "$SAVED_WS" >/dev/null 2>&1 || true

  local now
  now=$(hyprctl -j activeworkspace | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  if [[ "$now" == "$SAVED_WS" ]]; then
    echo "[shot:$LABEL] restored to ws=$now"
  else
    echo "[shot:$LABEL] WARNING: restore failed (on ws=$now, wanted $SAVED_WS)" >&2
  fi
  exit $rc
}
trap restore EXIT INT TERM

# --- reveal the test workspace if hidden -----------------------------------
if [[ "$SPECIAL_WAS_VISIBLE" == "no" ]]; then
  hyprctl dispatch togglespecialworkspace app-test >/dev/null 2>&1
  sleep 0.45
fi

# --- capture ---------------------------------------------------------------
grim "$OUT" || { echo "[shot:$LABEL] grim failed" >&2; exit 1; }
echo "[shot:$LABEL] wrote $OUT ($(stat -c %s "$OUT") bytes)"
