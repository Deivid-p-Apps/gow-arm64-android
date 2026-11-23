#!/data/data/com.termux/files/usr/bin/bash
# Build the recompiled game into a shared library (arm64) the app loads.
# Usage: tools/build_game_so.sh <game.exe> <out_dir> [driver]
# Produces <out_dir>/libgow_game.so with gow_func_table exported.
set -euo pipefail

EXE="${1:?usage: build_game_so.sh <game.exe> <out_dir> [clang]}"
OUT="${2:?usage: build_game_so.sh <game.exe> <out_dir> [clang]}"
CC="${3:-clang}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GEN="$OUT/gen"
OBJ="$OUT/obj"
mkdir -p "$GEN" "$OBJ"

echo "[1/3] lifting $EXE"
"$ROOT/target/release/gowrecomp" "$EXE" "$GEN"

echo "[2/3] compiling shards -> $OBJ"
shopt -s nullglob
for f in "$GEN"/gow_shard_*.c "$GEN"/gow_table.c; do
  o="$OBJ/$(basename "${f%.c}").o"
  echo "  cc $(basename "$f")"
  "$CC" -O2 -fPIC -fno-strict-aliasing -I "$ROOT/runtime/include" -I "$GEN" -c "$f" -o "$o"
done

echo "[3/3] linking libgow_game.so"
"$CC" -shared -fPIC -o "$OUT/libgow_game.so" "$OBJ"/*.o
echo "done: $OUT/libgow_game.so"
ls -la "$OUT/libgow_game.so"
