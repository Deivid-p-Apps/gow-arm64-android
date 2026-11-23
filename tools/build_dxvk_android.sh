#!/data/data/com.termux/files/usr/bin/bash
# Build DXVK Native (d3d11+dxgi -> Vulkan) for Android arm64.
# Phase 1 of the graphics layer: get the game on screen fast, then replace DXVK
# with a native Rust Vulkan backend (phase 2) once it runs.
#
# This mirrors the approach proven by the C&C Generals Zero Hour Android port:
#   - gate x86-only SSE intrinsics
#   - fix the SDL3 WSI soname
#   - high-DPI WSI patch (Android surfaces report real pixel size)
#
# Usage: tools/build_dxvk_android.sh <ndk> <out_dir>
# Requires: git, python3, meson, ninja, android NDK.
set -euo pipefail

NDK="${1:?usage: build_dxvk_android.sh <ndk-path> <out-dir>}"
OUT="${2:?usage: build_dxvk_android.sh <ndk-path> <out-dir>}"
WORK="${OUT}/dxvk-src"
mkdir -p "$OUT"

if [ ! -d "$WORK" ]; then
  git clone --depth 1 --recursive https://github.com/doitsujin/dxvk.git "$WORK"
fi

# Cross file for Android arm64 (adjust API level as needed).
cat > "$OUT/android-arm64.txt" <<EOF
[binaries]
c = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang'
cpp = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang++'
ar = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar'
strip = '$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip'

[host_machine]
system = 'linux'
cpu_family = 'aarch64'
cpu = 'aarch64'
endian = 'little'

[built-in options]
buildtype = 'release'
EOF

# Native (non-Wine) build with the d3d11/dxgi frontends and Vulkan backend.
meson setup "$OUT/build" "$WORK" \
  --cross-file "$OUT/android-arm64.txt" \
  -Denable_d3d9=false \
  -Denable_d3d10=false \
  -Denable_d3d11=true \
  -Denable_dxgi=true \
  -Denable_glslang=disabled \
  -Dnative=true 2>&1 | tail -5 || {
    echo "note: dxvk needs its Android patches (SSE gating, SDL3 WSI) — see android.md";
    exit 3;
  }

ninja -C "$OUT/build" -j4
echo "DXVK Native built: $OUT/build/src"
