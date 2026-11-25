#!/data/data/com.termux/files/usr/bin/bash
# Host boot harness build: compile the real recompiled game + runtime for the
# host (Termux aarch64), link a native runner. ~1-2 min with -O0.
set -e
cd /data/data/com.termux/files/home/gow-arm64
OUT=/data/data/com.termux/files/home/.cache/opencode/tmp/opencode/hostboot
rm -rf "$OUT/obj"; mkdir -p "$OUT/obj"
LOG="$OUT/build.log"; : > "$LOG"
OPT="${OPT:--O0}"
export OPT OUT LOG
ls generated/gow_shard_*.c generated/gow_table.c | xargs -P 7 -n1 sh -c '
  f="$1"; base=$(basename "$f" .c)
  clang "$OPT" -fPIC -fno-strict-aliasing -I runtime/include -I generated -c "$f" -o "$OUT/obj/$base.o" 2>> "$LOG" || echo "FAIL $f" >> "$LOG"
' _
cp generated/manifest.json "$OUT/manifest.json"
clang "$OPT" -I runtime/include -I generated -c tools/host_boot.c -o "$OUT/obj/host_boot.o" 2>> "$LOG"
clang "$OUT"/obj/*.o -L target/release -lgow_runtime -lpthread -ldl -lm -Wl,-rpath,"$PWD/target/release" -o "$OUT/host_boot" 2>> "$LOG"
echo "built: $(ls -la "$OUT/host_boot" 2>/dev/null || echo FAILED)"
