# ASSETS — data files of the game and how the port loads them

Reconnaissance of the retail data laid down by God of War (2018) PC. Every
claim below is tagged **CONFIRMED** (derived from bytes actually inspected) or
**HYPOTHESIS** (a plausible reading not yet proven). Nothing here is invented;
where a structure is not understood it is called out explicitly.

Companion tool: `tools/asset_probe.py` (bounded reads only; prints a header
summary for one file, or `unknown` when it does not recognise it).

Measurements were taken on:

```
/storage/emulated/0/god/GodOfWar/
  total on disk: 24,499,506,854 bytes = 22.8 GiB, 45,405 files
```

---

## 1. Layout and sizes

### 1.1 Top level (`<install>/`) — CONFIRMED

Executables, DLLs and the user-visible config. Nothing here is bulk game data.

| File | Size | Role |
|---|---:|---|
| `GoW.exe` | 19,833,344 | PE32+ x86-64 main executable (see `docs/NOTES.md`) |
| `settings.ini` | 2,753 | render/video/audio settings (CRLF ASCII) |
| `flt.ini` | 122 | Steam emu config: `AppId=1593500`, `Language=english`, `BuildId=7969425` |
| `steam_api64.dll` | 285,752 | Steamworks stub |
| `steamclient64.dll` | 11,832,320 | Steam client emu |
| `bink2w64.dll` | 372,224 | Bink2 video decoder (`.bk2`) |
| `nvngx_dlss.dll` | 14,464,720 | DLSS (bypassed at our quality settings) |
| `libSceGnm.dll` | 357,376 | Sony GNM→D3D11 shim |
| `libSceGpuAddress.dll` | 212,480 | Sony GPU address shim |
| `libSceJobManager.dll` | 137,216 | Sony job manager shim |
| `libScePad.dll` | 171,520 | Sony pad shim |
| `winmm.dll` | 544,256 | audio shim |
| `GoW.NoTAA.asi` | 65,024 | community no-TAA ASI plugin |
| `dxvk.conf`, `dxvk(2).conf`, `dxvk3).conf` | ~0.4–0.6 KB | DXVK configs (user-supplied) |
| `GoW_dxgi.log`, `GoW_d3d11.log` | 3,428 / 0 | DXVK logs |

> Note: there is **no** `boot-options.json` at the top level. The known
> `boot-options.json` lives under `exec/` (§2.1).

### 1.2 `exec/` subtree — CONFIRMED

Everything the game streams at runtime. ~23 GiB.

| Path | Direct files | Direct size | Contents |
|---|---:|---:|---|
| `exec/wad/pc_le/` | 1,416 | 17.7 GiB | 597 `.wad`, 597 `.as`, 19+19 pack `.toc`, 1 `.lodpack`, 1 `.texpack`, 1 `.shaderpack`, 179 `.wypdb`, `filehashes.csv`, 1 `.json` |
| `exec/wad/pc_le/soundbanks/` | 334 | 1.13 GiB | 334 `.sbp` at top; 12 language subdirs × 47 `.sbp` (898 `.sbp` total incl. subdirs) |
| `exec/sound/pc_le/` | 5,307 | 2.34 GiB | `.wem` at top; subdirs `english(us)` (12,351), `portuguese(brazil)` (12,349), `russian` (12,349) — 42,356 `.wem` total |
| `exec/cinematics/` | 129 | 152.3 MiB | `.bk2` Bink2 videos |
| `exec/dc/pc_le/` | 561 | 154.7 MiB | `.dcb` data containers |
| `exec/ActivityFeed/` | 14 | ~0 | story `.json` |
| `exec/languages/` | 1 | 359 B | `LANGS_GOWA08000.txt` |
| `exec/` top level | 9 | 2.0 MiB | `boot-options.json`, `profile.default.sp.int8.json`, `crs-*.dll/exe`, `PC_ICON_256.png`, licenses |

File-type histogram over the whole tree: 42,356 `.wem`, 898 `.sbp`,
597 `.as`, 597 `.wad`, 561 `.dcb`, 179 `.wypdb`, 129 `.bk2`, 38 `.toc`,
17 `.json`, plus exe/dll/ini/conf/log/csv/png.

### 1.3 Directory case vs. the executable's format strings — IMPORTANT

The executable builds its data paths from these literal format strings
(**CONFIRMED**, extracted from `GoW.exe` `.rdata`):

```
exec\wad\PC_LE\
exec\dc\PC_LE\
exec\wad\PC_LE\soundbanks\
exec\cinematics\
exec\languages\
exec\sound\PC_LE\
exec\configuration.pcpath
exec\debug\
%cprofile.user.%i.%s.%s.json      ; also profile.default.%s.%s.json
boot-options.json
./settings.ini  /  .\settings.ini
./exec/PC_ICON_256.png
```

But on disk every one of these directories is **lowercase** (`exec/wad/pc_le/`).
So the executable addresses `PC_LE` while the filesystem stores `pc_le`.

* On Windows this is harmless (case-insensitive).
* On the Android FUSE mount backing `/storage/emulated/0` the lookup
  `.../exec/wad/PC_LE/...` **succeeds** because that filesystem is
  case-insensitive too — verified: `os.path.exists(".../wad/PC_LE/root.lodpack")
  == True` while `os.listdir(".../wad") == ["pc_le"]`.
* This is a portability landmine: if the game directory is ever placed on a
  case-sensitive filesystem (e.g. an ext4 app-private copy), every game-asset
  open will fail. **The runtime must do ASCII-case-insensitive path resolution
  for reads** (see §4).

---

## 2. Config / text files (all CONFIRMED)

### 2.1 `exec/boot-options.json` (1,049 B)

Top-level manifest the launcher/loader reads first.

```json
{
 "is-package": true,
 "content-id": "PC0000-GOWA08000_00-00000000PLATINUM",
 "enable-trophies": true,
 "build-info": {
  "app-version": "01.35", "master-version": "01.00",
  "build-id": 79291, "wad-pack-id": 357668,
  "code-changelist": 4757534, "data-changelist": 4717343,
  "code-branch": "//int8-code/main81", "data-branch": "//int8-data/main81"
 },
 "playgo-chunks": ["root", "010_Forest_010", "en", "fr", ... ],
 "patch-texpacks": [],
 "patch-lodpacks": []
}
```

* `playgo-chunks` = logical content chunks; the `NNN_Region_NNN` names map 1:1
  to the per-chunk `<name>.lodpack.toc` / `<name>.texpack.toc` pairs in
  `exec/wad/pc_le/` (19 chunks on disk; the list also names language/media
  chunks `en`,`fr`,… that stream from `exec/sound` and `soundbanks`).
* `patch-*packs` are empty → no patch overlay packs to merge. If a GOTY/patch
  build fills these, the port must load the patched `.toc` on top of the base.
* `is-package: true` = the build is a PlayGo package; the loader parses these
  fields (the strings `is-package`, `playgo-chunks`, `patch-texpacks`,
  `patch-lodpacks` all appear in `GoW.exe`).

### 2.2 `exec/configuration.pcpath` — REFERENCED, ABSENT

`GoW.exe` contains the literal `exec\configuration.pcpath`. No such file exists
in this install. **HYPOTHESIS:** an optional override telling the loader where
the PC data root is; absent ⇒ defaults to the executable's own directory. The
port should simply tolerate its absence.

### 2.3 `exec/languages/LANGS_GOWA08000.txt` (359 B)

Maps Steam language id → 2–5 letter code → engine name → numeric id, e.g.
`1 en ENGLISH 1`, `17 pt-BR BR_PORTUGUESE 17`, `0 ja JAPANESE 0`. The code
column (`en`, `pt-BR`, …) selects `exec/sound/pc_le/<lang>/` and the matching
`soundbanks/<lang>/` directory; the language directory names on disk are the
long forms (`english(us)`, `portuguese(brazil)`, `spanish(spain)`), so the port
needs the mapping table, not a naive `code → dir` join.

### 2.4 `exec/wad/pc_le/filehashes.csv` (71,694 B, 3,106 lines)

`<64-bit hash>,<small int>` per line, no header (CONFIRMED: every line matches,
second field ∈ 0..30). **HYPOTHESIS:** content-integrity/verify list; the pack
`name_hash` fields (§2.6) are the same kind of 64-bit name hash. Not needed for
loading — can be skipped.

### 2.5 JSON assets

* `exec/ActivityFeed/*.json` (14) + `exec/ActivityFeed/stories.json` — CRLF
  text, plain JSON, ActivityFeed definitions (`"caption-lams-id": 43884`, …).
* `exec/profile.default.sp.int8.json`, `profile.user.<n>.<...>.json` — player
  profile (classes/xp/upgrades). Plain JSON, CRLF. Writable by the game.

---

## 3. Binary formats

Notation: all integers are **little-endian** unless stated. Offsets are absolute
from file start.

### 3.1 Pack TOCs — `.lodpack.toc` / `.texpack.toc` — **CONFIRMED**

A pack is `root.<kind>` (the big file) plus one `.toc` per (*kind*, chunk).
The `.toc` is **exactly the first `toc_size` bytes of the pack file** — verified
byte-for-byte for `root.lodpack`/`root.texpack`. So the runtime can read a pack's
directory by reading just the small `.toc`, then seek into the big body.

**`.lodpack.toc`** (also every per-chunk `NNN_*.lodpack.toc`; verified on
`root`, `010_forest_010`, `140_jotunheim_010`):

```
0x00 u32 A            number of "table A" entries
0x04 u32 B            number of "table B" entries
0x08 u32 0            (unused; 0 in every sample)
0x0C u32 1            (constant)
0x10                  Table A: A entries × 0x18 bytes
0x10 + A*0x18         Table B: B entries × 0x18 bytes
= file size exactly
```

*Table A* entry (0x18): `u64 offset, u64 name_hash, u32 size, u32 flag`.
  - `offset` is the payload offset **inside the pack body**; A[0].offset equals
    the TOC size (i.e. the first payload starts right after the directory).
  - entries tile the body: `A[i].offset + A[i].size == A[i+1].offset`, and
    `A[last].offset + A[last].size == pack_size` (CONFIRMED for all 1,419 entries
    of `root.lodpack`).
  - `flag` ∈ {0,1}.
*Table B* entry (0x18): `u32 a, u32 b, u64 name_hash, u32 size, u32 flag`, where
  **`a` is an index into Table A** (CONFIRMED: `a < A` in 100 % of entries, and
  the set of distinct `a` covers every A index, in `root` and both chunks).
  `b` is a second number whose meaning is not established.
  **HYPOTHESIS:** Table B is a name→(which-A-payload) lookup; Table A is the
  physical extent list.

**`.texpack.toc`** (verified on `root` + 4 chunks):

```
0x00..0x1F  zero (unused header, all bytes 0)
0x20 u32 self_size     == file length (CONFIRMED)
0x24 u32 count_like    7043 (root)
0x28 u32 data_off      start of the trailing data region
0x2C u32 entry_count
0x30 u32 5             constant in every sampled texpack
0x34 u32 0
0x38                   Entry table: entry_count × 0x18 bytes
0x38 + entry_count*0x18 == data_off      (CONFIRMED on all 5 samples)
```

*Entry* (0x18): `u64 h1, u64 h2, u32 offset, u32 flag`; `offset` is
non-decreasing and points into the pack body; `flag` was 0 in samples.
`h1`/`h2` are 64-bit hashes/ids (**HYPOTHESIS**: texture name hash + format id).

> **Not decoded:** the semantics of the trailing data region (texture metadata /
> mip tables) and of the `flag` fields. The port can already enumerate entries
> and slice payloads; interpreting the payloads (BC/BC7 texture headers) is
> still to be done.

### 3.2 `.as` records — **CONFIRMED**

`exec/wad/pc_le/<name>.as` is paired with a same-stem `<name>.wad`. 597 `.as`
exist; **403 are 0 bytes** (nothing to load) and 194 are non-empty. Every
non-empty `.as` is a whole multiple of 64 KiB.

Structure: a flat array of 0x10000-byte slots. An occupied slot at base `B` has
its record header at `B+0x10`; slots *inside* a large payload are zero-filled and
carry no header. Record header:

```
+0x00 u32 type      = 3                (constant across all 194 non-empty .as)
+0x04 u32 fill      = 0xE2E2E2E2        (constant marker half)
+0x08 u32 block_base = B                (the record's own slot base)
+0x0C u32 size      = payload size in bytes
+0x10 u64 name_hash (e.g. 0xEFF... for 'FreyaPrototype')
+0x18 char name[0x20]  NUL-padded ASCII  ('FreyaPrototype', 'Cal100_Hub', ...)
```

Confirmed on `r_perm.as` (157 occupied slots), `r_heroa00.as` (447), `r_son00.as`
(495): the `03 00 00 00 E2 E2 E2 E2` marker occurs only at `0x10, 0x10010,
0x20010, …`, always on a 0x10000 boundary. Per-record payload starts at
`B+0x10+0x30`-ish (**HYPOTHESIS** on the exact payload start; the header fields
are certain).

Biggest `.as`: `r_heroa00.as` 704,446,464 B; `r_son00.as` 489,422,848 B.

### 3.3 `.wad` — **CONFIRMED magic/constants; record layout HYPOTHESIS**

Header is identical across all 597 `.wad`:

```
0x00 u32 21 (0x15)            constant
0x04 u32 8                    constant
0x08 u8[12] AA AA AA BA AA AA AA AA AA AA AA AA   section signature
0x14 u32 tag                  e.g. 0xAAAD1187
0x18 char name[0x20]          e.g. 'WAD_Alf000_SkyDark'
0x38 ...                      (zero padding to 0x50)
0x50 char sentinel[16]        'YouLoveFromMaya\0'    (ASCII, repeated per record)
0x60 u32, 0x64 u32            LE fields
```

The 12-byte signature `..AA BA..` and the ASCII sentinel `YouLoveFromMaya`
recur throughout the file (`alf000_skydark.wad` alone contains 1,270 signature
markers and 2,920 sentinel hits). **HYPOTHESIS:** the `.wad` is a sequence of
records, each = signature + 0x20-byte name + sentinel + fields + payload, and the
`.as` is the sidecar that indexes them. Exact record header layout beyond the
name/sentinel is **not** decoded.

### 3.4 `root.shaderpack` — **CONFIRMED magic/version/count only**

```
0x00 char magic[8] = "SHDRPACK"
0x08 u64 version = 1
0x10 u64 count   = 26,606
0x18 u64 = 461,729,128
0x20 u64 = 8,054
0x28 u64 = 150,739,136
0x30 u64 = 75,508,688
0x38..0x60 zero
```

Body starts at 0x60; size = 76,410,832 B. **HYPOTHESIS:** the u64s at
0x18/0x20/0x28 are section offsets/counts (shader blobs and metadata); they do
not equal the body length, so their meaning is unproven.
**Not decoded:** the record format (an early guess of `u64 hash + u32s` did not
tile cleanly, so it is left open).

### 3.5 `.bk2` — Bink2 video — **CONFIRMED**

All 129 files start with `KB2j` (0x6A 0x32 0x42 0x4B). Header fields verified:

```
0x00 char magic[4] "KB2j"
0x04 u32 size_minus_8   == filesize - 8   (CONFIRMED on all 129)
0x08 u32 frame_count?   159
0x0C u32 max/?          16700
0x10 u32 frame_count    159   (same as 0x08 here)
0x14 u32 width          480
0x18 u32 height         270
0x1C u32 fps            24    (30 for the 4K sample)
0x20 u32 1
0x24 u32 16
0x28 u32 1
0x2C u32 (stream size)
0x30..   frame index table (u32 offsets, rising)
```

All 128 short clips are 480×270 (24 or 30 fps); the one 4K clip is
3840×2160@30. Decoding is done by `bink2w64.dll`; the port needs a Bink2
decoder (or must passthrough) — outside this task.

### 3.6 `.wem` — Wwise audio — **CONFIRMED**

Standard RIFF/WAVE with a 66-byte `fmt ` chunk and Wwise's tag `0xFFFF`:

```
0x00 "RIFF"; 0x04 u32 riff_size == filesize-8
0x08 "WAVE"
0x0C "fmt "; 0x10 u32 chunk_len = 66
0x14 u16 wFormatTag = 0xFFFF (Wwise Vorbis); 0x16 u16 channels
0x18 u32 sampleRate = 48000 (sampled); 0x1C u32 avgBytes/sec
0x24 u32 = 0x30 (48) — Wwise fmt extension
0x56 "data"; 0x5A u32 data_size
```

The 66-byte `fmt` confirms Wwise Vorbis-in-WEM. **Not decoded:** the Vorbis
setup in the extension (needed for a from-scratch decoder; a vendored Wwise
decoder would read it natively).

### 3.7 `.sbp` — Wwise soundbank — **header CONFIRMED, record layout HYPOTHESIS**

```
0x00 u32 = 0x00050001   (constant in every sampled bank, both langs)
0x04 u32 total_size     (bank size; not equal to filesize in all samples)
0x08 u8[16] = DD DD DD DD AA AA AA AA 11 11 11 11 EE EE EE EE   fill/sentinel
0x18 char name[0x20]    'SBPW_vo_glb_baldur_banter'
```

334 banks at `soundbanks/` top level + 47 per language × 12 = 898 total.
**HYPOTHESIS:** Wwise SoundBank derivative keyed by `name`. Record chunk layout
not decoded.

### 3.8 `.dcb` — data container — **header CONFIRMED, layout HYPOTHESIS**

All 561 `.dcb` share the leading header:

```
0x00 u16 11      (0x000B)
0x02 u16 16      (0x0010)
0x04 u32 4
0x08..0x17 zero
0x18 char name[0x20]   'DC_bestiary', 'DC_characterfx', ...
```

**HYPOTHESIS:** an internal archive whose entries repeat the `DC_<name>` label;
a 12,176-byte file references `DC_bestiary` five times. Not decoded.

### 3.9 `.wypdb` — pipeline-DB sidecar — **HYPOTHESIS**

179 files, one per some `.wad`/`.as` stem (e.g. `alf150_bridgedark.wypdb`).

```
0x00 u16 1
0x02 u16 85
0x04 u32 size (file-108 in sample)
0x08 u64 hash
0x10 u64 hash
0x18 char name[0x20]  'Alf150_BridgeDark'
... chunk tag 'RLTD' appears at 0x98 and 0x38F8
```

Only the header fields and the `RLTD` tag are CONFIRMED. **HYPOTHESIS:** an
offline pipeline/tweak database used by tooling, possibly not read at runtime.

### 3.10 `filehashes.csv` and text — **CONFIRMED**

See §2.4. `.ini`/`.conf`/`.txt`/`.log` are ASCII (CRLF in `settings.ini`).

---

## 4. How the game locates files, and what the runtime must do

### 4.1 Path model (CONFIRMED from `GoW.exe` strings + on-disk tree)

The executable composes data paths as `<base>\exec\...` using the literal pieces
in §1.3, with `\` separators and the uppercase `PC_LE`. `<base>` is the
executable's own directory (no absolute path is baked in; `GetModuleFileNameA/W`,
`GetFullPathNameW`, `SetCurrentDirectory*` are imported). It also reads:
`./settings.ini`, `.\settings.ini`, `./exec/PC_ICON_256.png`,
`exec\wad\PC_LE\soundbanks\`, `exec\sound\PC_LE\`, `exec\cinematics\`,
`exec\languages\`, `exec\dc\PC_LE\`, plus `<profile>.json`.

The runtime already maps Windows paths into the game directory
(`runtime/src/fileio.rs::FileTable::translate`: backslash→slash, strip the
`C:` drive, join under `game_dir`). The Apollo app sets `game_dir` to
`/storage/emulated/0/god/GodOfWar`; note the on-disk root is the *parent* of
`exec/`, so `<base>` = that directory, exactly as the exe assumes.

### 4.2 Gaps the runtime must close for asset loading

1. **Case-insensitive reads.** The exe opens `exec\wad\PC_LE\...`; the on-disk
   name is `pc_le`. This works today only because the FUSE mount is
   case-insensitive. `FileTable::translate` joins the raw string, so on a
   case-sensitive FS it would fail. Add an ASCII-case-insensitive resolver (walk
   each path component, match names case-insensitively) for **read/exists**
   paths. Writes may stay exact.
2. **Memory-mapped files.** `GoW.exe` imports **`CreateFileMappingA`,
   `MapViewOfFile`, `UnmapViewOfFile`** — but `runtime/src` has **no**
   implementation of any of them (0 references). The multi-hundred-MB packs will
   almost certainly be mapped, not read in chunks. The runtime must implement
   mapping over the host file (e.g. `mmap`), returning a guest pointer to the
   mapped bytes. `UnmapViewOfFile` must drop it. If a pack is mapped at a fixed
   guest address the port's guest-address model (D004) must be respected.
3. **`GetFullPathNameW`, `GetFileAttributesExA`, `FindFirstFileExA`** are
   imported and not implemented. `FindFirstFileExA` returns `WIN32_FIND_DATA`
   (the existing `find_first`/`find_next` covers the `A` variant's common
   fields; add the `Ex` entry point and the `A` struct variant).
4. **`GetPrivateProfileStringA` / `WritePrivateProfileStringA`** are imported
   (INI access). `settings.ini` is read through them; the runtime currently has
   no INI backend. Either implement keyed INI reads or pre-seed the values.
5. **Large-file I/O.** `ReadFile` currently does `vec![0u8; len]` per call; for
   multi-MB streaming reads that is hot. Keep it correct, but a mapping path
   (§4.2.2) is the real answer.
6. **`.toc` vs body.** For every pack, read the small `*.toc` for the directory;
   seek/map into `root.<kind>` for payloads. The TOC is a byte-for-byte prefix of
   the pack, so a single mapping of the pack exposes the TOC too.
7. **Lod/language selection.** Use `boot-options.json` `playgo-chunks` to know
   which chunk TOCs exist, and `LANGS_GOWA08000.txt` (+ `flt.ini Language=`) to
   pick the `sound/pc_le/<lang>` and `soundbanks/<lang>` subtrees.

### 4.3 What still blocks actual asset use

Even with the above, on-screen use of wads/texpacks needs the payload codecs
(textures, meshes, shaders, audio), which are **not** decoded here. This document
covers *finding and framing* the files — the part the filesystem layer needs —
and leaves payload decoding to the renderer/audio milestones.

---

## 5. Confidence summary

| Format | Status | What is certain |
|---|---|---|
| `boot-options.json`, `flt.ini`, `settings.ini`, `LANGS_*.txt`, `*.json`, `*.csv` | **CONFIRMED** | text/JSON/CSV, contents as shown |
| `.lodpack.toc` (root + chunks) | **CONFIRMED** | header counts, 0x18 entries, A tiles the body, B.r0 indexes A |
| `.texpack.toc` (root + chunks) | **CONFIRMED** | self-size, entry count at 0x2C, `0x38+N*0x18==data_off`, entry = 2×u64+u32+u32 |
| `.lodpack` / `.texpack` body | **CONFIRMED** | body = TOC prefix + payloads; payload semantics unknown |
| `.shaderpack` | **CONFIRMED magic/ver/count**; record layout HYPOTHESIS |
| `.as` | **CONFIRMED** | 64 KiB slots; header `type/fill/base/size/hash/name[0x20]` at slot+0x10 |
| `.wad` | **CONFIRMED** constants/signature/name/sentinel; record layout HYPOTHESIS |
| `.bk2` | **CONFIRMED** | `KB2j`, size-8, dims, fps, frame table |
| `.wem` | **CONFIRMED** | RIFF/WAVE, Wwise fmt 0xFFFF, 66-byte fmt, data chunk |
| `.sbp` | **CONFIRMED** header; record layout HYPOTHESIS |
| `.dcb` | **CONFIRMED** header; record layout HYPOTHESIS |
| `.wypdb` | header **HYPOTHESIS** (name + `RLTD` tag certain) |
| Texture/mesh/shader/audio payload internals | **UNKNOWN** | out of scope here |
