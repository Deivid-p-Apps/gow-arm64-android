#!/data/data/com.termux/files/usr/bin/bash
cd /data/data/com.termux/files/home/gow-arm64
mkdir -p game_obj
for f in generated/gow_shard_*.c generated/gow_table.c; do
  o=game_obj/$(basename "${f%.c}").o
  [ -f "$o" ] && continue
  clang -O1 -fPIC -fno-strict-aliasing -I runtime/include -I generated -c "$f" -o "$o" 2>>game_build_err.log
done
# link only when all present
n=$(ls game_obj/gow_shard_*.o 2>/dev/null | wc -l)
if [ "$n" -eq 36 ] && [ -f game_obj/gow_table.o ]; then
  clang -shared -fPIC -Wl,-soname,libgow_game.so -o libgow_game.so game_obj/*.o -L target/release -lgow_runtime >>game_build_err.log 2>&1
  echo "LINKED ok size=$(ls -la libgow_game.so 2>/dev/null|awk '{print $5}')" > build_status.txt
else
  echo "INCOMPLETE shards=$n/36" > build_status.txt
fi
