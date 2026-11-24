#!/usr/bin/env bash
# Build DXVK Native (d3d11 + dxgi -> Vulkan) for Android arm64 (Route B: Android WSI).
#
# This is the LOCAL convenience wrapper. The authoritative, repeatable build is
# the `dxvk` job in .github/workflows/build.yml (ubuntu runner, 16 GB RAM); see
# docs/DXVK_ANDROID.md §10 for the verified recipe. Run this on a capable host
# (not the phone) with an NDK + meson/ninja/glslang:
#
#   tools/build_dxvk_android.sh <ndk-path> <out-dir> [api-level]
#
# Requires: git, meson (>=1.0), ninja, python3, glslang, patchelf(optional), NDK r27.
set -euo pipefail

NDK="${1:?usage: build_dxvk_android.sh <ndk-path> <out-dir> [api]}"
OUT="${2:?usage: build_dxvk_android.sh <ndk-path> <out-dir> [api]}"
API="${3:-29}"
DXVK_TAG="${DXVK_TAG:-v3.1.1}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$OUT/dxvk-src"
mkdir -p "$OUT"

if [ ! -d "$WORK" ]; then
  # dxbc-spirv + libdisplay-info are submodules with no .wrap fallback.
  git clone --recursive --depth 1 --branch "$DXVK_TAG" \
    https://github.com/doitsujin/dxvk.git "$WORK"
else
  git -C "$WORK" submodule update --init --recursive --depth 1
fi

# Apply the Android patches (P1-P6). Idempotent: skip ones already applied.
for p in "$REPO_ROOT"/tools/patches/*.patch; do
  if git -C "$WORK" apply --check "$p" 2>/dev/null; then
    git -C "$WORK" apply "$p"
    echo "applied $(basename "$p")"
  else
    echo "skipped (already applied?): $(basename "$p")"
  fi
done

TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
cat > "$OUT/android-arm64.ini" <<EOF
[binaries]
c          = '$TC/aarch64-linux-android${API}-clang'
cpp        = '$TC/aarch64-linux-android${API}-clang++'
ar         = '$TC/llvm-ar'
strip      = '$TC/llvm-strip'
pkg-config = 'pkg-config'

[properties]
needs_exe_wrapper = false

[host_machine]
system     = 'android'
kernel     = 'linux'
cpu_family = 'aarch64'
cpu        = 'aarch64'
endian     = 'little'

[built-in options]
c_args        = ['-fPIC']
cpp_args      = ['-fPIC']
c_link_args   = ['-Wl,-z,max-page-size=16384']
cpp_link_args = ['-Wl,-z,max-page-size=16384']
EOF

# There is NO -Dnative=true; native mode is chosen by system != 'windows'.
# system='android' + the P3 patch select the Android WSI and drop the SDL/GLFW need.
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

D3D11="$OUT/build/src/d3d11/libdxvk_d3d11.so"
DXGI="$OUT/build/src/dxgi/libdxvk_dxgi.so"
echo "DXVK Native (Android WSI) built:"
ls -la "$D3D11" "$DXGI"
echo "--- SONAME / NEEDED ---"
"$TC/llvm-readelf" -d "$DXGI"  | grep -E 'SONAME|NEEDED' || true
"$TC/llvm-readelf" -d "$D3D11" | grep -E 'SONAME|NEEDED' || true
echo "--- exports ---"
"$TC/llvm-readelf" --dyn-syms -W "$DXGI"  | grep -E 'CreateDXGIFactory1|dxvk_android_set_window'
"$TC/llvm-readelf" --dyn-syms -W "$D3D11" | grep -E 'D3D11CreateDevice|dxvk_android_set_window'

# Strip debug info before packaging; copy to stable unversioned names.
mkdir -p "$OUT/dist"
cp "$D3D11" "$OUT/dist/libdxvk_d3d11.so"
cp "$DXGI"  "$OUT/dist/libdxvk_dxgi.so"
"$TC/llvm-strip" --strip-debug "$OUT/dist/libdxvk_d3d11.so" "$OUT/dist/libdxvk_dxgi.so"
echo "Packaged into $OUT/dist; copy into android/app/src/main/jniLibs/arm64-v8a/."
