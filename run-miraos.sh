#!/bin/sh
# Build and boot native Mira code, not the Rust reference kernel or Linux IDE.
set -eu
MIRA_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$MIRA_ROOT"
case "${1:-}" in
  ''|--build|--screen|--headless) ;;
  *)
    printf 'usage: %s [--build|--screen|--headless]\n' "$0" >&2
    exit 2
    ;;
esac
# The ESP/runtime directory is shared, even when a different state disk is used.
mkdir -p "$MIRA_ROOT/target/miraos"
exec 9>"$MIRA_ROOT/target/miraos/session.lock"
if ! flock -n 9; then
  printf 'MiraOS is already running or being built from this project. Close the existing QEMU window before starting another session.\n' >&2
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
  PATH="$HOME/.cargo/bin:$PATH"
  export PATH
fi
case "${1:-}" in
  --build)
    python3 tools/prepare-miraos.py --rebuild
    ;;
  ''|--screen|--headless)
    printf 'MiraOS starts autonomously. Console: :s status, :e memory, :p pause, :r reset, :q halt.\n'
    MIRA_STATE_DISK=${MIRA_STATE_DISK:-${XDG_DATA_HOME:-$HOME/.local/share}/miraos/experience-v2.img}
    export MIRA_STATE_DISK
    printf 'State image: %s\n' "$MIRA_STATE_DISK"
    printf 'Exit QEMU: Ctrl+A, then X. Use :q and Enter for a final checkpoint.\n'
    python3 tools/prepare-miraos.py
    if [ "${1:-}" = --screen ]; then
      printf 'Click the QEMU screen to type through its USB keyboard. :b edits; Ctrl+S saves; Ctrl+X returns. This terminal remains a fallback.\n'
      target/debug/mira boot-uefi target/miraos/esp
    else
      target/debug/mira boot-uefi target/miraos/esp --headless
    fi
    ;;
esac
