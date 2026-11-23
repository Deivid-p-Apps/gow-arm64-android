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

```sh
# 1) analyze the executable (M0)
python3 tools/pe_probe.py "/path/to/GoW.exe"

# 2) build the recompiler and lift the whole game to C
cargo build -p gow-recompiler --release
./target/release/gowrecomp "/path/to/GoW.exe" generated
#  -> generated/gow_game.c , generated/manifest.json
```

CI builds the recompiler and an arm64 debug APK; download the artifact and test on
the connected device (`adb install -r`, logcat, screencap) — the device is the test rig.

## Status

See `docs/STATUS.md`. Current phase: M0/M1 — the lifter runs over the real executable
(51,869 functions, 2,756,013 instructions) and emits C with an explicit, non-silent
coverage report. Graphics/runtime layers are next.

## Legal

Unofficial fan project for preservation and study. Requires your own legally obtained
copy of the game. Not affiliated with or endorsed by Santa Monica Studio, Sony, or any
rights holder. No game assets are included or distributed.
