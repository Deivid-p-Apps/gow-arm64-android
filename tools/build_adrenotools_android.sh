#!/usr/bin/env bash
# Build libadrenotools + its hook libraries for Android arm64.
#
# The authoritative build is the `adrenotools` job in .github/workflows/build.yml
# (ubuntu runner + NDK r27 + CMake). This is the LOCAL convenience wrapper; run it
# on a capable host, not the phone.
#
#   tools/build_adrenotools_android.sh <ndk-path> <out-dir> [api-level]
#
# Produces, in <out-dir>/dist/ (copy them into
# android/app/src/main/jniLibs/arm64-v8a/ so they land in lib/arm64-v8a/):
#
#   libadrenotools.so        the library the Rust runtime dlopens
#   libmain_hook.so          the hook adrenotools loads into the loader's namespace
#   libhook_impl.so          shared implementation of that hook
#   libfile_redirect_hook.so only needed with ADRENOTOOLS_DRIVER_FILE_REDIRECT
#   libgsl_alloc_hook.so     only needed with ADRENOTOOLS_DRIVER_GPU_MAPPING_IMPORT
#
# libadrenotools is BSD-2-Clause (https://github.com/bylaws/libadrenotools).
set -euo pipefail

NDK="${1:?usage: build_adrenotools_android.sh <ndk-path> <out-dir> [api]}"
OUT="${2:?usage: build_adrenotools_android.sh <ndk-path> <out-dir> [api]}"
API="${3:-29}"
PIN="${ADRENOTOOLS_COMMIT:-8fae8ce254dfc1344527e05301e43f37dea2df80}"
WORK="$OUT/adrenotools-src"
mkdir -p "$OUT"

if [ ! -d "$WORK/.git" ]; then
  # --recursive pulls lib/linkernsbypass, the linker-namespace bypass the hook needs.
  git clone --recursive https://github.com/bylaws/libadrenotools.git "$WORK"
fi
git -C "$WORK" fetch origin "$PIN" || true
git -C "$WORK" checkout --detach "$PIN"
git -C "$WORK" submodule update --init --recursive
echo "libadrenotools @ $(git -C "$WORK" rev-parse HEAD)"

cmake -S "$WORK" -B "$OUT/build" -G Ninja \
  -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
  -DANDROID_ABI=arm64-v8a \
  -DANDROID_PLATFORM="android-$API" \
  -DANDROID_STL=c++_static \
  -DBUILD_SHARED_LIBS=ON \
  -DCMAKE_BUILD_TYPE=Release
cmake --build "$OUT/build" --parallel "$(nproc)"

TC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
DEST="$OUT/dist"
mkdir -p "$DEST"
cp "$OUT/build/libadrenotools.so" "$DEST/"
cp "$OUT/build/src/hook/libhook_impl.so" "$DEST/"
cp "$OUT/build/src/hook/libmain_hook.so" "$DEST/"
cp "$OUT/build/src/hook/libfile_redirect_hook.so" "$DEST/"
cp "$OUT/build/src/hook/libgsl_alloc_hook.so" "$DEST/"
"$TC/llvm-readelf" --dyn-syms -W "$DEST/libadrenotools.so" | grep -q adrenotools_open_libvulkan
"$TC/llvm-readelf" --dyn-syms -W "$DEST/libmain_hook.so" | grep -q android_dlopen_ext
"$TC/llvm-strip" --strip-debug "$DEST"/*.so
echo "Packaged into $DEST; copy into android/app/src/main/jniLibs/arm64-v8a/."
