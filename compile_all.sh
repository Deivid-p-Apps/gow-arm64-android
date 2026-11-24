#!/data/data/com.termux/files/usr/bin/bash
cd /data/data/com.termux/files/home/gow-arm64
ok=0; fail=0
for f in generated/gow_shard_*.c generated/gow_table.c; do
  o=game_obj/$(basename "${f%.c}").o
  if clang -O1 -fPIC -fno-strict-aliasing -I runtime/include -I generated -c "$f" -o "$o" 2>>game_build_err.log; then ok=$((ok+1)); else fail=$((fail+1)); echo "FAIL $f" >> game_build_err.log; fi
  echo "done $(basename $f)" >> compile_progress.txt
done
clang -shared -fPIC -Wl,-soname,libgow_game.so -o libgow_game.so game_obj/*.o -L target/release -lgow_runtime >>game_build_err.log 2>&1
echo "RESULT ok=$ok fail=$fail link=$? size=$(ls -la libgow_game.so 2>/dev/null|awk '{print $5}')" >> compile_progress.txt
