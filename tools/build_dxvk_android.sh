#!/data/data/com.termux/files/usr/bin/bash
# Build DXVK Native (d3d11 + dxgi -> Vulkan) for Android arm64 (Route B: Android WSI).
#
# Phase 1 of the graphics layer. Verified against DXVK v3.1.1 sources; the
# Android-WSI route avoids a permanent SDL3/GLFW dependency and takes the
# ANativeWindow from our SurfaceView directly. See docs/DXVK_ANDROID.md for the
# full plan and tools/dxvk_android_patches.md for the patch inventory (P0-P9).
#
# Usage: tools/build_dxvk_android.sh <ndk-path> <out-dir> [api-level]
# Requires: git, meson, ninja, python3, and the NDK.
set -euo pipefail

NDK="${1:?usage: build_dxvk_android.sh <ndk-path> <out-dir> [api]}"
OUT="${2:?usage: build_dxvk_android.sh <ndk-path> <out-dir> [api]}"
API="${3:-29}"
DXVK_TAG="${DXVK_TAG:-v3.1.1}"
WORK="$OUT/dxvk-src"
mkdir -p "$OUT"

if [ ! -d "$WORK" ]; then
  git clone --recursive --depth 1 --branch "$DXVK_TAG" https://github.com/doitsujin/dxvk.git "$WORK"
else
  git -C "$WORK" submodule update --init --recursive
fi

TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
cat > "$OUT/android-arm64.ini" <<EOF
[binaries]
c = '$TC/aarch64-linux-android${API}-clang'
cpp = '$TC/aarch64-linux-android${API}-clang++'
ar = '$TC/llvm-ar'
strip = '$TC/llvm-strip'

[host_machine]
system = 'android'
kernel = 'linux'
cpu_family = 'aarch64'
cpu = 'aarch64'
endian = 'little'

[built-in options]
buildtype = 'release'
c_args = ['-Wl,-z,max-page-size=16384']
cpp_args = ['-Wl,-z,max-page-size=16384']
EOF

# Route B disables SDL/GLFW WSI and uses the Android WSI backend (P3/P4).
# NOTE: the option names are native_sdl2 / native_sdl3 / native_glfw in DXVK 3.x.
# (There is NO -Dnative=true; that was an early draft mistake.)
# The P1..P6 patches from tools/dxvk_android_patches.md must be applied to $WORK
# first (this script documents the configure step; patching is manual/reviewed).
meson setup "$OUT/build" "$WORK" \
  --cross-file "$OUT/android-arm64.ini" \
  --buildtype release \
  -Dnative_sdl2=disabled \
  -Dnative_sdl3=disabled \
  -Dnative_glfw=disabled \
  -Denable_d3d8=false \
  -Denable_d3d9=false \
  -Denable_d3d10=false \
  -Denable_d3d11=true \
  -Denable_dxgi=true

ninja -C "$OUT/build" src/d3d11/libdxvk_d3d11.so src/dxgi/libdxvk_dxgi.so
echo "DXVK Native (Android WSI) built:"
ls -la "$OUT/build/src/d3d11/libdxvk_d3d11.so" "$OUT/build/src/dxgi/libdxvk_dxgi.so"
