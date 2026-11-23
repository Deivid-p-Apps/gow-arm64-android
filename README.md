# gow-arm64-android

Static x86-64 → aarch64 recompilation of **God of War (2018) PC** to native Android (arm64-v8a).

No JIT. No Wine. No emulator at play time. The game's x86-64 code is translated
ahead of time to C, compiled natively for arm64, and runs on the device's own CPU/GPU.

The repository contains **no game code or game files**. You supply your own PC copy;
the recompiler turns it into native code locally. Everything generated from the game
stays out of this repo (see `.gitignore`).

## Layout

| Path | Contents |
|---|---|
| `recompiler/` | Rust: PE32+ parser, function discovery (.pdata/entry/TLS), x86-64 → C lifter (iced-x86) |
| `runtime/` | Rust: guest dispatcher, register file, guest memory, control-flow map; Win32/NT + D3D11→Vulkan layer (in progress) |
| `runtime/include/gow_guest.h` | ABI seen by generated C |
| `tools/pe_probe.py` | PE reconnaissance (headers, sections, imports, exports, .pdata, TLS) |
| `docs/` | STATUS.md, DECISIONS.md, NOTES.md |
| `.github/workflows/build.yml` | CI: builds recompiler and the Android APK artifact |

## Build & use

### Recompiler (host)

```sh
# 1) analyze the executable (M0)
python3 tools/pe_probe.py "/path/to/GoW.exe"

# 2) build the recompiler and lift the whole game to C
cargo build -p gow-recompiler --release
./target/release/gowrecomp "/path/to/GoW.exe" generated
#  -> generated/gow_shard_NNN.c (35 shards), gow_table.c, gow_protos.h
```

### Build the recompiled game into a native library

The recompiled game becomes a shared library the app loads at runtime — it is
never committed (it is derived from your game files):

```sh
tools/build_game_so.sh "/path/to/GoW.exe" out
#  -> out/gen/*.c  and  out/libgow_game.so   (arm64 when built with the NDK clang)
```

On desktop, `clang` produces a host `.so` useful for testing the dispatcher; for
the device, pass the NDK clang (e.g. `aarch64-linux-android34-clang`) as the
third argument.

### APK

CI builds `android/app` into a debug APK (artifact `gow-debug-apk`), which ships
the runtime (`libgow_runtime.so`), the driver loader glue, the Java activity and
the manifest. Test on the connected device:

```sh
adb install -r app-debug.apk
adb shell appops set com.gow.arm64 MANAGE_EXTERNAL_STORAGE allow
adb shell am start -n com.gow.arm64/.GowActivity
adb logcat | grep GoW
```

The app reads the game files from `/storage/emulated/0/god/` at runtime and loads
the user-built `libgow_game.so` from its private files directory. Custom Turnip
drivers are loaded from `/storage/emulated/0/god/drivers/` (adrenotools package:
`meta.json` + `libvulkan_freedreno.so`).

### End-to-end control-flow test

```sh
python3 tools/make_flow_pe.py > flow.bin
./target/release/gowrecomp flow.bin flowout
clang -O2 -I runtime/include -I flowout tests/flow_runner.c \
  flowout/gow_shard_000.c flowout/gow_table.c \
  -L target/release -lgow_runtime -lpthread -Wl,-rpath,target/release -o flow_runner
./flow_runner   # prints rax=15
```

CI builds the recompiler and an arm64 debug APK; download the artifact and test on
the connected device (`adb install -r`, logcat, screencap) — the device is the test rig.

## Status

See `docs/STATUS.md` for the phase, what works, what is missing, and known bugs.
`docs/DECISIONS.md` records engineering choices, `docs/NOTES.md` the reverse
engineering, `docs/PERFORMANCE.md` the measurement methodology, and
`docs/GRAPHICS.md` the D3D11->Vulkan plan.

Current phase: M1/M2 — the recompiler lifts the whole executable (51,869
functions, 2,754,847 instructions, 99.987% coverage), the generated C compiles
with the Android clang, control flow is proven end-to-end, a real arm64 APK
builds in CI and runs on the connected device, and the runtime provides the
dispatcher, memory, integer/flags, SSE/SSE2/AVX subset, x87, Win32 file/thread/
sync layer, and a Vulkan + Turnip driver bootstrap. Still to come: the D3D11
renderer, audio, input and the game reaching its main.

## Legal

Unofficial fan project for preservation and study. Requires your own legally obtained
copy of the game. Not affiliated with or endorsed by Santa Monica Studio, Sony, or any
rights holder. No game assets are included or distributed.
