#!/data/data/com.termux/files/usr/bin/bash
# Host boot harness build: compile the real recompiled game + runtime for the
# host (Termux aarch64), link a native runner. ~1-2 min with -O0.
#
# INCREMENTAL by default: an existing .o newer than its .c is reused, so a build
# after a runtime-only change relinks in seconds instead of recompiling 215
# shards. The Android device has little free RAM, and clang -O0 on a shard needs
# ~200 MB, so parallelism is capped (JOBS, default 2) to avoid the low-memory
# killer taking down the whole Termux session.
set -e
cd /data/data/com.termux/files/home/gow-arm64
OUT=/data/data/com.termux/files/home/.cache/opencode/tmp/opencode/hostboot
mkdir -p "$OUT/obj"
LOG="$OUT/build.log"; : > "$LOG"
OPT="${OPT:--O0}"
JOBS="${JOBS:-2}"
export OPT OUT LOG
# Recompile only shards whose .o is missing or older than the .c.
ls generated/gow_shard_*.c generated/gow_table.c | xargs -P "$JOBS" -n1 sh -c '
  f="$1"; base=$(basename "$f" .c); o="$OUT/obj/$base.o"
  if [ -f "$o" ] && [ "$o" -nt "$f" ]; then exit 0; fi
  clang "$OPT" -fPIC -fno-strict-aliasing -I runtime/include -I generated -c "$f" -o "$o" 2>> "$LOG" || echo "FAIL $f" >> "$LOG"
' _
cp generated/manifest.json "$OUT/manifest.json"
clang "$OPT" -I runtime/include -I generated -c tools/host_boot.c -o "$OUT/obj/host_boot.o" 2>> "$LOG"
clang "$OUT"/obj/*.o -L target/release -lgow_runtime -lpthread -ldl -lm -Wl,-rpath,"$PWD/target/release" -o "$OUT/host_boot" 2>> "$LOG"
echo "built: $(ls -la "$OUT/host_boot" 2>/dev/null || echo FAILED)"
