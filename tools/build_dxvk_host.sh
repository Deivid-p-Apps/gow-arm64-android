#!/usr/bin/env bash
# Build DXVK Native for the Termux host with the SDL2 WSI.
#
# The APK ships Android-WSI libraries (vkCreateAndroidSurfaceKHR + ANativeWindow),
# which cannot present to the X11 display used by the host harness. This builds
# the same DXVK version with the SDL2 WSI instead: the guest's CreateWindowExA is
# served by runtime/src/sdl2win.rs, which creates a real SDL2 window and returns
# it as the HWND, so DXVK's SDL2 WSI can create an Xlib Vulkan surface.
#
# Requirements (Termux): clang, meson, ninja, sdl2, glslang, pkg-config, python3.
#   pkg install ninja sdl2 glslang
#   pip install meson
#
# Usage: tools/build_dxvk_host.sh [src-dir] [build-dir]
set -euo pipefail

SRC="${1:-$HOME/.cache/opencode/tmp/opencode/dxvk-ref}"
BUILD="${2:-$SRC/build-host}"
JOBS="${JOBS:-1}"
TAG="${DXVK_TAG:-v3.1.1}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ ! -d "$SRC" ]; then
  git clone --recursive --depth 1 --branch "$TAG" \
    https://github.com/doitsujin/dxvk.git "$SRC"
fi
git -C "$SRC" submodule update --init --recursive --depth 1

# Termux has no /usr/bin/env; the code generators use it as a shebang.
if ! [ -e /usr/bin/env ]; then
  while IFS= read -r f; do
    sed -i "1s|^#!/usr/bin/env.*|#!$(command -v python3)|" "$f"
  done < <(grep -rl '^#!/usr/bin/env' "$SRC" --include='*.py' || true)
fi

# Relax DXVK's device-feature requirements for Adreno/Turnip (same patch the
# Android build uses). Idempotent.
P="$ROOT/tools/patches/0006-dxvk-relax-required-features-for-adreno.patch"
if git -C "$SRC" apply --check "$P" 2>/dev/null; then
  git -C "$SRC" apply "$P"
  echo "applied $(basename "$P")"
fi

if [ ! -f "$BUILD/build.ninja" ]; then
  meson setup "$BUILD" "$SRC" \
    --buildtype=plain -Dc_args=-O1 -Dcpp_args=-O1 \
    -Dnative_sdl2=enabled -Dnative_sdl3=disabled -Dnative_glfw=disabled \
    -Denable_d3d8=false -Denable_d3d9=false -Denable_d3d10=false \
    -Denable_d3d11=true -Denable_dxgi=true
fi

ninja -C "$BUILD" -j"$JOBS" \
  src/d3d11/libdxvk_d3d11.so src/dxgi/libdxvk_dxgi.so

DIST="${GOW_DXVK_DIR:-/storage/emulated/0/god/dxvk-host}"
mkdir -p "$DIST"
cp "$BUILD/src/d3d11/libdxvk_d3d11.so" "$BUILD/src/dxgi/libdxvk_dxgi.so" "$DIST/"
ls -la "$DIST"/libdxvk_*.so
