#!/data/data/com.termux/files/usr/bin/bash
cd /data/data/com.termux/files/home/gow-arm64
rm -rf game_obj game_build_err.log game_build_done.txt
mkdir -p game_obj
ok=0; fail=0
for f in generated/gow_shard_*.c generated/gow_table.c; do
  o=game_obj/$(basename "${f%.c}").o
  if clang -O1 -fPIC -fno-strict-aliasing -I runtime/include -I generated -c "$f" -o "$o" 2>>game_build_err.log; then
    ok=$((ok+1))
  else
    fail=$((fail+1)); echo "FAIL $f" >> game_build_err.log
  fi
done
echo "ok=$ok fail=$fail" > game_build_done.txt
# link into the game .so
clang -shared -fPIC -o libgow_game.so game_obj/*.o >>game_build_err.log 2>&1 && echo "linked libgow_game.so" >> game_build_done.txt || echo "LINK_FAILED" >> game_build_done.txt
