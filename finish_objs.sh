#!/data/data/com.termux/files/usr/bin/bash
cd /data/data/com.termux/files/home/gow-arm64
for i in 001 006 008 017 019 024 026 027 028 029 030 031; do
  f=generated/gow_shard_$i.c; o=game_obj/gow_shard_$i.o
  [ -f "$o" ] && continue
  clang -O1 -fPIC -fno-strict-aliasing -I runtime/include -I generated -c "$f" -o "$o" 2>>game_build_err.log
done
# link if all present
missing=0
for i in $(seq -w 0 34); do [ -f "game_obj/gow_shard_0$i.o" ] || missing=$((missing+1)); done
if [ "$missing" = 0 ] && [ -f game_obj/gow_table.o ]; then
  clang -shared -fPIC -o libgow_game.so game_obj/*.o >>game_build_err.log 2>&1
  echo "linked: $(ls -la libgow_game.so 2>/dev/null | awk '{print $5}') bytes" > finish_done.txt
else
  echo "still missing $missing shards" > finish_done.txt
fi
