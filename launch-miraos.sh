#!/bin/sh
# Applications-menu entry point. Native input is supplied by QEMU's USB keyboard.
set -eu
MIRA_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$MIRA_ROOT"
MIRA_DATA_DIR=${XDG_DATA_HOME:-$HOME/.local/share}/miraos
MIRA_LOG_DIR=${XDG_STATE_HOME:-$HOME/.local/state}/miraos
MIRA_STATE_DISK=${MIRA_STATE_DISK:-$MIRA_DATA_DIR/experience-v2.img}
export MIRA_STATE_DISK

open_path() {
  if command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$1" >/dev/null 2>&1 || true
  fi
}

notify() {
  if command -v notify-send >/dev/null 2>&1; then
    notify-send --app-name=MiraOS MiraOS "$1" >/dev/null 2>&1 || true
  fi
}

case "${1:-}" in
  --context)
    exec xdg-open "$MIRA_ROOT/MIRA_CONTEXT.md"
    ;;
  ''|--screen|--export) ;;
  *)
    printf 'usage: %s [--screen|--context|--export]\n' "$0" >&2
    exit 2
    ;;
esac

# Reopening the menu entry should reveal this checkout's existing guest.
if [ "${1:-}" != --export ] && command -v hyprctl >/dev/null 2>&1; then
  if python3 - "$MIRA_ROOT" <<'PY'
import json
from pathlib import Path
import re
import subprocess
import sys

try:
    clients = json.loads(subprocess.check_output(['hyprctl', 'clients', '-j'], stderr=subprocess.DEVNULL))
    for window in clients:
        if window.get('class') != 'qemu':
            continue
        arguments = Path(f"/proc/{int(window['pid'])}/cmdline").read_bytes()
        if (sys.argv[1] + '/target/miraos/').encode() not in arguments:
            continue
        address = window.get('address', '')
        if not re.fullmatch(r'0x[0-9a-fA-F]+', address):
            continue
        result = subprocess.run(['hyprctl', 'dispatch',
            f'hl.dsp.focus({{window="address:{address}"}})'], capture_output=True, text=True)
        if result.returncode == 0 and result.stdout.strip() == 'ok':
            sys.exit(0)
except (OSError, ValueError, KeyError, subprocess.SubprocessError):
    pass
sys.exit(1)
PY
  then
    exit 0
  fi
fi

umask 077
mkdir -p "$MIRA_LOG_DIR"
MIRA_STAMP=$(date -u +%Y%m%d-%H%M%S)-$$
MIRA_LOG=$MIRA_LOG_DIR/launch-$MIRA_STAMP.log
if [ "${1:-}" = --export ]; then
  MIRA_LOG=$MIRA_LOG_DIR/export-$MIRA_STAMP.log
fi
printf 'MiraOS\nState image: %s\nProject: %s\n\n' "$MIRA_STATE_DISK" "$MIRA_ROOT" >"$MIRA_LOG"

if [ "${1:-}" = --export ]; then
  MIRA_EXPORT_DIR=$MIRA_DATA_DIR/exports
  mkdir -p "$MIRA_EXPORT_DIR"
  MIRA_EXPORT=$MIRA_EXPORT_DIR/MiraOS-saved-$MIRA_STAMP.mira
  if python3 "$MIRA_ROOT/tools/export-miraos-source.py" "$MIRA_STATE_DISK" "$MIRA_EXPORT" >>"$MIRA_LOG" 2>&1; then
    notify "Saved source exported to $MIRA_EXPORT"
    open_path "$MIRA_EXPORT_DIR"
    printf '%s\n' "$MIRA_EXPORT"
    exit 0
  fi
  printf '\nThe saved source could not be exported. Save the draft inside MiraOS with Ctrl+S, then try again. The state image has not been changed.\n' >>"$MIRA_LOG"
else
  printf 'Click the QEMU window to type. :b opens the editor; Ctrl+S saves; Ctrl+X returns. Use :q and Enter to save a final checkpoint, then close the QEMU window.\n\n' >>"$MIRA_LOG"
  if "$MIRA_ROOT/run-miraos.sh" --screen </dev/null >>"$MIRA_LOG" 2>&1; then
    exit 0
  fi
  printf '\nMiraOS could not start or stopped with an error. If another MiraOS window is already open, close that session before launching again. Otherwise check the error above; Cargo, QEMU and OVMF must be installed.\n' >>"$MIRA_LOG"
fi
printf 'MiraOS: see %s\n' "$MIRA_LOG" >&2
notify "The action could not finish. Details: $MIRA_LOG"
open_path "$MIRA_LOG"
exit 1
