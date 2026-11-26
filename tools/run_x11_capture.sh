#!/usr/bin/env bash
# Run the recompiled God of War on the host with the selected graphics backend
# and capture the X11 screen (the progress screenshot).
#
# Usage: tools/run_x11_capture.sh [game-dir] [screenshot.png]
#   DUR=25           seconds before the first capture
#   DUR2=45          seconds before the second capture (0 = skip)
#   BACKEND=dxvk|native
#   DISPLAY=:1
set -uo pipefail

GAME="${1:-/storage/emulated/0/god}"
SHOT="${2:-$GAME/Port/execucao-$(date +%H%M%S).png}"
BACKEND="${BACKEND:-dxvk}"
DUR="${DUR:-25}"
DUR2="${DUR2:-45}"
export DISPLAY="${DISPLAY:-:1}"
export LD_LIBRARY_PATH="$PREFIX/lib:${LD_LIBRARY_PATH:-}"
export VK_ICD_FILENAMES="${VK_ICD_FILENAMES:-$PREFIX/share/vulkan/icd.d/freedreno_icd.aarch64.json}"
export GOW_X11_DISPLAYS="${GOW_X11_DISPLAYS:-$DISPLAY}"
if [ "$BACKEND" = native ]; then
  export GOW_GFX_BACKEND=native
else
  unset GOW_GFX_BACKEND || true
  export GOW_DXVK_WSI="${GOW_DXVK_WSI:-SDL2}"
  # /storage/emulated/0 (FUSE) is not dlopen-able by the Termux linker, so the
  # host DXVK libraries live under $HOME (see tools/build_dxvk_host.sh).
  export GOW_DXVK_DIR="${GOW_DXVK_DIR:-$HOME/.cache/opencode/tmp/opencode/hostboot/dxvk-host}"
fi

HB="${HB:-$HOME/.cache/opencode/tmp/opencode/hostboot/host_boot}"
[ -x "$HB" ] || { echo "host_boot not found at $HB"; exit 2; }

# Make sure the display exists (start Xvfb :1 if needed).
if ! xdpyinfo >/dev/null 2>&1; then
  echo "starting Xvfb $DISPLAY"
  (Xvfb "${DISPLAY}" -screen 0 1280x720x24 >/dev/null 2>&1 &)
  sleep 2
fi

mkdir -p "$(dirname "$SHOT")"
rm -f "$GAME"/Port/gow-*.log
echo "[run] backend=$BACKEND display=$DISPLAY game=$GAME"
"$HB" "$GAME" >/dev/null 2>&1 &
PID=$!

sleep "$DUR"
import -window root "$SHOT" 2>/dev/null
echo "[shot] $SHOT"
if [ "${DUR2:-0}" -gt "$DUR" ] 2>/dev/null; then
  sleep $((DUR2 - DUR))
  SHOT2="${SHOT%.png}-2.png"
  import -window root "$SHOT2" 2>/dev/null
  echo "[shot] $SHOT2"
fi

kill -9 "$PID" 2>/dev/null
wait "$PID" 2>/dev/null
L="$(ls -t "$GAME"/Port/gow-*.log 2>/dev/null | head -1)"
if [ -n "$L" ]; then
  echo "[log] $L ($(wc -l <"$L") lines)"
  grep -aE "D3D11CreateDevice|CreateSwapChain|Present|gow.gfx|gow.sdl2|gow.window|_initterm ran|thread\] start" "$L" | tail -15
fi
ls -la "$SHOT" ${SHOT2:+"$SHOT2"} 2>/dev/null
