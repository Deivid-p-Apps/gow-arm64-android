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

## Progress map

Legend: ✅ done · 🟡 partial · ⬜ not started · 🔒 blocked on game files/hardware

```
M0  Reconnaissance                     ✅  PE32+ parsed; 7 sections, 579 imports, 61 exports,
                                           51,869 functions in .pdata; device identified
M1  Loader + recompiler                ✅  lift 51,869 funcs / 2,754,847 insns, coverage 99.9919%
                                           (224 traps); generated C compiles (clang -O2, arm64);
                                           control flow proven end-to-end (rax=15 test)
M2  Runtime reaches game main          🟡  dispatcher, guest mem/stack, exact int flags, SSE/SSE2/
                                           AVX subset, x87, MMX; Win32 layer (files/threads/
                                           sync/heap/time); Steamworks + Sony stubs
M3  Window + graphics device           🟡  Vulkan bootstrap + android surface + Turnip driver load;
                                           DXVK Native integration in progress
M4  Main menu renders                  ⬜
M5  Gameplay playable                  ⬜
M6  50+ FPS stable                     ⬜  (hardware ceiling on Adreno 725 — see PERFORMANCE.md)
M7  Native Rust Vulkan backend         ⬜  (phase 2; DXVK is the phase-1 fallback)
M8  Polish / robustness                ⬜
```

```
Recompiler     [####################] 100%   (whole executable lifted, .so links)
Runtime core   [################----]  80%   (M2: Win32 layer + COM hostcall in progress)
Graphics       [######--------------]  30%   (M3: Vulkan up; DXVK not yet building)
Audio          [########------------]  40%   (XAudio2->AAudio engine + mixer; not wired to device)
Input          [##########----------]  50%   (XInput->Android model + JNI; overlay pending)
Android shell  [############--------]  60%   (APK builds/installs/runs; game .so inside)
CI             [################----]  80%   (recompiler + e2e test + APK; DXVK build not in CI)
Cache/measure  [--------------------]   0%   (no FPS numbers yet — nothing fabricated)
```

Live numbers and the honest limits are in [`docs/STATUS.md`](docs/STATUS.md),
[`docs/PERFORMANCE.md`](docs/PERFORMANCE.md), [`docs/GRAPHICS.md`](docs/GRAPHICS.md).

## Status

See `docs/STATUS.md` for the phase, what works, what is missing, and known bugs.
`docs/DECISIONS.md` records engineering choices, `docs/NOTES.md` the reverse
engineering, `docs/PERFORMANCE.md` the measurement methodology, and
`docs/GRAPHICS.md` the D3D11->Vulkan plan.

Current phase: M1 complete, M2/M3 in progress — the recompiler lifts the whole
executable (51,869 functions, 2,754,847 instructions, 99.9919% coverage), the
generated C compiles with the Android clang, control flow is proven end-to-end,
a real arm64 APK builds in CI and runs on the connected device, and the runtime
provides the dispatcher, memory, integer/flags, SSE/SSE2/AVX subset, x87, the
Win32 file/thread/sync layer, a COM hostcall mechanism, the XAudio2→AAudio engine
and XInput→Android input, plus a Vulkan + Turnip driver bootstrap. Still to come:
the D3D11 renderer (DXVK Native), the swapchain, wiring audio/input to the device,
and the game reaching its main.

## Legal

Unofficial fan project for preservation and study. Requires your own legally obtained
copy of the game. Not affiliated with or endorsed by Santa Monica Studio, Sony, or any
rights holder. No game assets are included or distributed.
