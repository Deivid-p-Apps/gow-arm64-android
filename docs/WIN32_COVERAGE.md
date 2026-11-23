# WIN32 COVERAGE — imports used by `GoW.exe` vs. what the runtime implements

Measured with `tools/imports_report.py` against the real executable
(`/storage/emulated/0/god/GodOfWar/GoW.exe`, PE32+ x64, image_base `0x140000000`).
The "implemented" set is **parsed from the runtime source**, not hand-written:
the `("dll", "func")` arms of `runtime/src/win32.rs::ImportResolver::call`, the
wildcard DLL arms (`("dll", f)` / `("dll", _)`), and the
`graphics.rs::D3D_BOOTSTRAP` table.

Reproduce:
```
python3 tools/imports_report.py /storage/emulated/0/god/GodOfWar/GoW.exe --coverage
python3 tools/imports_report.py /storage/emulated/0/god/GodOfWar/GoW.exe --json
```

## Headline numbers

| metric | value |
|---|---|
| total imported functions (IAT entries) | **579** |
| DLLs imported | **38** |
| handled by `ImportResolver::call` | **72** |
| missing (no handler) | **507** |
| coverage | **12.4 %** |

> **Structural caveat (read this first).** `ImportResolver::call` is the only
> Win32 entry point, reached from lifted code via `gow_import_call`
> (`runtime/src/dispatch.rs:452`). That path fetches the resolver from a
> thread-local that is **only ever set by `dispatch::set_import_resolver`**
> (`dispatch.rs:470`) and that function is **called from nowhere in the tree**;
> likewise `ImportResolver::load_manifest` has no caller, and `GowCpu::import_map`
> is never populated. So today **zero imports actually reach the implementation
> below** — every import call returns with RAX untouched and bumps the `MISSING`
> counter. Wiring the resolver into the run path (`jni.rs` /
> `run_game_library`) is the single prerequisite for every number in this doc to
> matter. The counts below describe the surface that *would* be handled once the
> resolver is installed.

## Per-DLL coverage

| DLL | imports | impl | missing | what it is |
|---|---:|---:|---:|---|
| KERNEL32.dll | 146 | 62 | 84 | process/memory/file/thread/console core |
| USER32.dll | 64 | 5 | 59 | windowing, messages, raw input, DPI |
| VERSION.dll | 2 | 0 | 2 | file-version resource reads |
| XINPUT1_4.dll | 2 | 2 | 0 | gamepad (ord#2/#3) |
| dxgi.dll | 1 | 1 | 0 | swapchain factory bootstrap |
| d3d11.dll | 1 | 1 | 0 | device creation bootstrap |
| D3DCOMPILER_47.dll | 1 | 1 | 0 | shader reflection bootstrap |
| MSVCP140.dll | 10 | 0 | 10 | MSVC C++ runtime (`std::`) |
| IPHLPAPI.DLL | 1 | 0 | 1 | adapter enumeration |
| VCRUNTIME140.dll | 21 | 0 | 21 | MSVC C runtime (mem/str/C++ EH) |
| api-ms-win-crt-stdio-l1-1-0.dll | 41 | 0 | 41 | CRT stdio |
| api-ms-win-crt-runtime-l1-1-0.dll | 32 | 0 | 32 | CRT startup/exit |
| api-ms-win-crt-string-l1-1-0.dll | 39 | 0 | 39 | CRT str/wcs/ctype |
| api-ms-win-crt-heap-l1-1-0.dll | 8 | 0 | 8 | CRT allocator |
| api-ms-win-crt-utility-l1-1-0.dll | 4 | 0 | 4 | qsort/bsearch/rand |
| api-ms-win-crt-convert-l1-1-0.dll | 11 | 0 | 11 | CRT numeric conversion |
| api-ms-win-crt-environment-l1-1-0.dll | 2 | 0 | 2 | getenv/putenv |
| api-ms-win-crt-math-l1-1-0.dll | 34 | 0 | 34 | CRT libm |
| api-ms-win-crt-time-l1-1-0.dll | 5 | 0 | 5 | CRT time |
| api-ms-win-crt-filesystem-l1-1-0.dll | 4 | 0 | 4 | CRT stat/splitpath |
| api-ms-win-crt-locale-l1-1-0.dll | 2 | 0 | 2 | locale |
| api-ms-win-crt-multibyte-l1-1-0.dll | 2 | 0 | 2 | mbcs helpers |
| libScePad.dll | 9 | 0 | 9 | Sony pad (PS4 shim) |
| WLDAP32.dll | 16 | 0 | 16 | LDAP (ordinals) |
| WSOCK32.dll | 30 | 0 | 30 | legacy Winsock (ordinals) |
| libSceJobManager.dll | 7 | 0 | 7 | Sony job/thread pool (PS4 shim) |
| bink2w64.dll | 14 | 0 | 14 | Bink video decoder |
| libSceGnm.dll | 24 | 0 | 24 | Sony GNM texture/dataformat (PS4 shim) |
| libSceGpuAddress.dll | 8 | 0 | 8 | Sony GPU tiling helpers (PS4 shim) |
| WS2_32.dll | 4 | 0 | 4 | getaddrinfo/getnameinfo |
| steam_api64.dll | 9 | 0 | 9 | Steamworks |
| GDI32.dll | 2 | 0 | 2 | text/bk color |
| ADVAPI32.dll | 7 | 0 | 7 | registry + process token |
| SHELL32.dll | 1 | 0 | 1 | known-folder path |
| ole32.dll | 7 | 0 | 7 | COM apartments + task mem |
| OLEAUT32.dll | 2 | 0 | 2 | ord#8 / ord#9 (BSTR) |
| SETUPAPI.dll | 5 | 0 | 5 | device enumeration |
| RPCRT4.dll | 1 | 0 | 1 | UuidCreate |
| **TOTAL** | **579** | **72** | **507** | |

### What "implemented" actually means today (72 entries)

* **KERNEL32 (62)** — `VirtualAlloc/Free`, `VirtualProtect/Query`,
  `GetModuleHandleA/W/ExA`, `GetProcAddress`, `GetLastError`, heap
  (`GetProcessHeap`, `HeapAlloc/Free/ReAlloc/Size`…), file I/O
  (`CreateFileA/W`, `ReadFile`, `WriteFile`, `SetFilePointer[Ex]`,
  `GetFileSize[Ex]`, `CloseHandle`, `FindFirst/NextFile[A/W]`, `FindClose`,
  `GetFileAttributes[A/W]`, `CreateDirectory[A/W]`, `DeleteFileA`,
  `GetModuleFileName[A/W]`, `GetCommandLineA/W`), threading/sync
  (`CreateThread`, `CreateMutexA`, `CreateEventA/W`, `Set/ResetEvent`,
  `WaitForSingleObject`, `WaitForMultipleObjects`, `ReleaseMutex`,
  `Enter/Leave/Delete/InitializeCriticalSection[AndSpinCount]`,
  `GetCurrentThread[Id]`, `GetCurrentProcess[Id]`, `SwitchToThread`,
  `SetThreadPriority`), time (`QueryPerformanceCounter/Frequency`, `Sleep`,
  `GetTickCount[64]`, `GetSystemTime[AsFileTime]`), `GetSystemInfo`,
  `SetUnhandledExceptionFilter`, `IsDebuggerPresent`, `GetVersionExA`,
  `LoadLibraryA/W`, `FreeLibrary`.
* **USER32 (5)** — `GetAsyncKeyState`/`GetKeyState`, `GetCursorPos`,
  `ShowCursor`, `SetCursorPos`, `ClipCursor`, `GetForegroundWindow`.
* **XINPUT1_4 (2)** — `ord#2` (XInputGetState), `ord#3` (XInputSetState),
  routed to the Android input layer.
* **D3D bootstrap (3)** — `d3d11!D3D11CreateDevice`,
  `dxgi!CreateDXGIFactory1`, `d3dcompiler_47!D3DReflect`: handled **only if a
  backend `.so` was `dlopen`ed**; otherwise each returns `E_NOINTERFACE`
  (`win32.rs:320-336`). These are "routed", not "working".

Honest notes on semantics of things counted as implemented: `GetProcAddress`
always returns 0 (`win32.rs:349` `resolve_proc`); `LoadLibraryA/W` return 0, so
any *dynamic* loading of e.g. `xaudio2_7.dll` fails; `LoadLibrary`-loaded code
paths are dead. `WaitForMultipleObjects` returns `WAIT_OBJECT_0` without
actually waiting (fake-signal). `VirtualAlloc` returns a **host** heap pointer
rather than guest-space memory. `GetLastError` is hard-wired to 0. These are the
documented stubs, not silent fakes, but they are the ones that will misbehave on
the real boot path.

## Discrepancies with `docs/STATUS.md` / `docs/NOTES.md` (measured, not assumed)

* `STATUS.md` lists `bcrypt` and `crypt32` as key imports. **Neither is in the
  IAT** (0 hits for `bcrypt`/`crypt32` module names). `RPCRT4!UuidCreate` is the
  only crypto-ish import. The shim layer's `docs` claim is wrong here.
* Networking is `WS2_32` (4) **plus** legacy `WSOCK32` (30, ordinals),
  `WLDAP32` (16, ordinals) and `IPHLPAPI!GetAdaptersInfo` (1) — 51 entries, not
  just `ws2_32`.
* `XAudio2` is handled by the runtime (`win32.rs:307`, wildcard DLLs) but is
  **not in the IAT at all** — no `xaudio2_*.dll` import exists. Audio either goes
  through another path or is loaded dynamically (which the current
  `LoadLibraryA/W = 0` prevents). The XAudio2 support is currently unreachable
  for this EXE.
* `steam.rs` implements `steam_api64()`, `sce()` and `xinput()` helpers, but
  **none of them is called from `ImportResolver::call`**; the `steam_api64` and
  `sce` modules are therefore entirely dormant. `steam_api64.dll` shows 0/9.
* `.pdata`/entry/TLS values in `NOTES.md` match (51 869 functions, entry
  `0x3ec680`, TLS callbacks present).

## There is no delay-import table

Only the standard import directory is present (no `DELAYIMP` data directory,
no `bcrypt`/`crypt32`/`xaudio2` descriptors). Every one of the 579 entries above
is loaded eagerly at process start, so all of them must resolve (or be safely
stubbed) before `main` can be reached.

---

## Most important MISSING functions, grouped by what they enable

"boot?" = does the CRT startup / WinMain path plausibly call it before the game
reaches its own `main`/first frame.

### 1. CRT startup & exit — `api-ms-win-crt-runtime-l1-1-0.dll` (32 missing)
This DLL *is* the pre-`main` sequence. Nothing works until these run.
| function | what it does | boot? |
|---|---|---|
| `_set_app_type` | declares GUI vs console app to the CRT | **yes** |
| `_initterm_e` / `_initterm` | run C then C++ static initializers (walks a function-pointer table) | **yes** |
| `_configure_narrow_argv`, `_initialize_narrow_environment` | build `argv`/environment | **yes** |
| `_get_narrow_winmain_command_line` | WinMain command line | **yes** |
| `_execute_onexit_table`, `_initialize_onexit_table`, `_register_onexit_function`, `_crt_atexit` | atexit/onexit machinery | **yes** |
| `exit`, `_exit`, `_c_exit`, `_cexit` | process teardown | yes (exit) |
| `_set_fmode`, `_configthreadlocale`, `_set_abort_behavior`, `_set_invalid_parameter_handler`, `_invalid_parameter_noinfo[_noreturn]` | CRT defaults / error hooks | yes |
| `_controlfp` | FPU control word | yes |
| `_getpid`, `_errno`, `__sys_nerr`, `strerror` | errno/errno strings | yes |
| `_beginthreadex`, `_endthreadex` | CRT threads (game threads) | yes |
| `_register_thread_local_exe_atexit_callback`, `_seh_filter_exe`, `_seh_filter_dll` | TLS/SEH startup hooks | yes |
| `abort`, `terminate`, `set_terminate`, `_crt_at_quick_exit` | termination paths | on failure |

### 2. CRT heap — `api-ms-win-crt-heap-l1-1-0.dll` (8 missing)
`malloc`, `free`, `calloc`, `realloc`, `_aligned_malloc`, `_aligned_realloc`,
`_aligned_free`, `_set_new_mode`. **Every** allocation in the C++ glue goes
through these; no boot without them. Bionic `malloc/free` map 1:1; aligned
variants need explicit alignment. | boot? **yes**

### 3. CRT stdio — `api-ms-win-crt-stdio-l1-1-0.dll` (41 missing)
`__acrt_iob_func` (the `stdin/stdout/stderr` FILE accessor), `fopen[_s]`,
`freopen`, `fread`, `fwrite`, `fseek/_fseeki64`, `ftell/_ftelli64`,
`fclose`, `fflush`, `fgets`, `fputs`, `fputc`, `getc`, `feof`, `ferror`,
`rewind`, `setvbuf`, `_read`/`_write`/`_open`/`_close`/`_lseeki64`/`_telli64`,
`_fileno`, `_get_osfhandle`, `_filelengthi64`, `_stat64`-adjacent helpers,
`__stdio_common_vsprintf[_s]`, `__stdio_common_vfprintf[_s]`,
`__stdio_common_vswprintf[_s]`, `__stdio_common_vsscanf`, `_setmaxstdio`,
`_set_fmode`, `_p__commode`, `_mktemp_s`, `_fsopen`, `_wfsopen`, `_telli64`.
This is both the CRT stream plumbing **and** how the game reads config/loose
files. | boot? **yes** (iob func, `_get_osfhandle`, `fopen` for logs/config)

### 4. CRT string / ctype — `api-ms-win-crt-string-l1-1-0.dll` (39 missing)
`strcmp`, `strncmp`, `strcpy_s`, `strncpy[_s]`, `strncat`, `strspn`, `strpbrk`,
`strcoll`, `strtok`, `strstr`(elsewhere), `_stricmp`, `_strnicmp`, `_strdup`,
`_strlwr`, `_strupr`, `strcat`-family, the `wcs*` equivalents
(`wcscpy_s`, `wcsncpy[_s]`, `wcscat_s`, `wcsncat_s`, `wcstok`, `wcsnlen`,
`_wcsicmp`, `_wcsnicmp`, `_wcsdup`, `_wcsupr`), and the ctype predicates
(`isdigit`, `isalpha`, `isspace`, `tolower`, `toupper`, `ispunct`, `isprint`,
`isupper`, `islower`, `iscntrl`, `isxdigit`, `isgraph`, `isalnum`). Almost all
trivially map to Bionic; the `_s` (bounds-checked) variants need the
invalid-parameter hook. | boot? **yes**

### 5. CRT convert / math / time / environment / filesystem / utility / locale / multibyte (105 missing)
* `convert` (11): `atoi`, `atof`, `strtod`, `strtol`, `strtoul`, `strtof`,
  `_strtoi64`, `_itoa`, `mbstowcs`, `wcstombs`, `wcsrtombs`.
* `math` (34): libm — `pow[f]`, `sin[f]`, `cos[f]`, `tan[f]`, `asin[f]`,
  `acos[f]`, `atan[f]`, `atan2f`, `sinh[f]`, `cosh[f]`, `tanh[f]`, `exp[f]`,
  `exp2f`, `log`, `log2f`, `log10[f]`, `fmod[f]`, `frexp`, `ldexp`, `modf[f]`,
  `cbrtf`, `roundf`, `_finite`, `_fdclass`, `_fdtest`, `__setusermatherr`.
* `time` (5): `_time64`, `_localtime64[_s]`, `_gmtime64`, `strftime`.
* `environment` (2): `getenv`, `_putenv`.
* `filesystem` (4): `_stat64`, `_fstat64`, `_splitpath[_s]`.
* `utility` (4): `qsort`, `bsearch`, `rand`, `srand`.
* `locale` (2): `localeconv`, `_configthreadlocale`.
* `multibyte` (2): `_mbsnbcpy`, `_mbschr`.
> All boot-adjacent: the CRT calls `getenv`, `_time64`, `qsort`, `mbstowcs`
> during init and the game calls the rest constantly.

### 6. VCRUNTIME140.dll (21 missing) — mem/str + C++ exception machinery
| function | what it does | boot? |
|---|---|---|
| `memcpy`, `memmove`, `memset`, `memcmp`, `memchr` | libc primitives, called by CRT before anything else | **yes** |
| `strstr`, `strchr`, `strrchr`, `wcsstr`, `wcschr`, `wcsrchr` | string search | yes |
| `longjmp`, `__intrinsic_setjmp` | C setjmp/longjmp (the CRT uses it) | yes |
| `__C_specific_handler`, `__CxxFrameHandler3`, `__CxxThrowException` (`_CxxThrowException`) | x64 SEH + C++ throw/catch dispatch | yes (any `try`) |
| `__std_terminate`, `_set_purecall_handler`, `_purecall`, `set_unexpected` | terminate/purecall hooks | on error |
| `__std_type_info_destroy_list` | RTTI teardown | yes |
> The status doc already admits **SEH (x64 exceptions) is not modelled**; these
> imports are the concrete blocker. A `throw` anywhere in boot (Steam, COM,
> file open) currently cannot be caught.

### 7. MSVCP140.dll (10 missing) — MSVC C++ standard library
`_Mtx_init_in_situ`, `_Mtx_destroy_in_situ`, `_Mtx_lock`, `_Mtx_unlock`
(`std::mutex` in-situ, i.e. `std::mutex` objects the game embeds),
`?_MP_Add/Mul/Rem/Get@std@@` (arbitrary-precision integer helpers used by
`std::chrono`/formatting), `?_Throw_C_error@std@@`, `?_Xlength_error@std@@`.
`_Mtx_*` are needed for any `std::mutex`/`std::lock_guard` in static
constructors. | boot? **yes** (`_Mtx_*` during static init)

### 8. Memory management — KERNEL32 (subset of the 84 missing)
| function | what it does | boot? |
|---|---|---|
| `GetSystemInfo`-adjacent: `GlobalMemoryStatusEx`, `K32GetProcessMemoryInfo`, `GetLargePageMinimum` | RAM/working-set queries the engine uses to size pools | yes |
| `LocalAlloc`, `LocalFree`, `LocalSize` | legacy local heap (still used by some libs) | yes |
| `CreateFileMappingA`, `MapViewOfFile`, `UnmapViewOfFile` | memory-mapped files / shared memory (asset streaming, IPC) | maybe |
| `HeapSetInformation`-adjacent already handled | — | — |

### 9. Threading / synchronisation — KERNEL32 (subset)
`CreateSemaphoreA`, `ReleaseSemaphore`, `WaitForSingleObjectEx`,
`WaitForMultipleObjectsEx`, `SleepEx`, `TryEnterCriticalSection`,
`InitializeCriticalSectionEx`, `InitializeSListHead`, `ResumeThread`,
`GetThreadId`, `SetThreadAffinityMask`, `GetThreadContext`,
`SetThreadContext`, `TerminateThread`, `CreateWaitableTimerA`,
`SetWaitableTimer`, `DuplicateHandle`, `CreateJobObjectA`,
`AssignProcessToJobObject`, `SetInformationJobObject`, `SetThreadExecutionState`,
and the **fiber API** `CreateFiber`, `ConvertThreadToFiber`,
`ConvertFiberToThread`, `SwitchToFiber`, `DeleteFiber`.
> The fiber imports are notable: some engines (and MSVC's own exception unwinder)
> use fibers. Threads exist (`threads.rs`), but the *sync primitives the game
> actually imports* (`Semaphore`, `*Ex` variants, `TryEnter`) do not.

### 10. Process / exit / error / SEH support — KERNEL32 (subset)
`ExitProcess`, `TerminateProcess`, `CreateProcessA/W`, `SetLastError`,
`DebugBreak`, `RaiseException`, `GetStdHandle`, `GetFileType`,
`GetStartupInfoW`, `QueryFullProcessImageNameA`, `GetSystemDirectoryW`,
`GetFullPathNameW`, `UnmapViewOfFile`, `MoveFileExA`, `GetFileTime`,
`SetFileTime`, `DeviceIoControl`, `RtlCaptureContext`, `RtlLookupFunctionEntry`,
`RtlVirtualUnwind`.
> `GetStartupInfoW` and `SetLastError` are called by CRT startup; the `Rtl*`
> trio are the x64 unwinder primitives.

### 11. Files (path/attributes) — KERNEL32
`GetFileAttributesExA`, `FindFirstFileExA`, `GetFullPathNameW`,
`GetDiskFreeSpaceExW`, `GetPrivateProfileStringA`,
`WritePrivateProfileStringA`, `GetFileType`. The engine's loose-file/config
lookups hit these.

### 12. String encoding — KERNEL32
`WideCharToMultiByte` and `MultiByteToWideChar` (the game is `W`-centric; the
runtime currently only handles the few ASCII paths), plus `FormatMessageA/W`,
`lstrlenW`, `lstrcmpA`, `GetUserDefaultLocaleName`,
`ExpandEnvironmentStringsA`.

### 13. Windowing / messages / input — USER32 (59 missing)
`RegisterClassExA`, `CreateWindowExA`, `DefWindowProcA`, `ShowWindow`,
`AdjustWindowRect`, `GetClientRect`, `GetWindowRect`, `SetWindowPos`,
`PeekMessageA`, `TranslateMessage`, `DispatchMessageA`, `PostQuitMessage`,
`SetWindowLongPtrW`/`GetWindowLongPtrW`/`SetWindowLongA`/`GetWindowLongA`,
`LoadCursorA`, `LoadIconA`, `SetCapture`/`ReleaseCapture`/`GetCapture`,
`SetWindowPlacement`/`GetWindowPlacement`, `InvalidateRect`, `BeginPaint`/
`EndPaint`, `ClientToScreen`/`ScreenToClient`, `MonitorFromWindow`,
`EnumDisplayMonitors`, `EnumDisplayDevicesA`, `EnumDisplaySettingsA`,
`GetMonitorInfoA`, `GetAncestor`, `IsWindow`, `PtInRect`,
`SetProcessDpiAwarenessContext`, `SetThreadDpiAwarenessContext`,
`RegisterRawInputDevices`, `GetRawInputData`, `GetCursorInfo`,
`GetWindowThreadProcessId`, `MapVirtualKeyA/ExA`, `VkKeyScanA`,
`LoadKeyboardLayoutA`, `GetKeyboardLayout`, `RegisterHotKey`,
`UnregisterHotKey`, `RegisterDeviceNotificationW`,
`UnregisterDeviceNotification`, `TrackMouseEvent`, `SendMessageA`,
`MessageBoxA/W`.
> A window must exist before the render loop; the message pump
> (`PeekMessageA`/`DispatchMessageA`) drives the whole frame. The runtime has a
> `SurfaceView`/ANativeWindow path, so these can be backed by an Android window
> rather than real Win32, but they are currently unimplemented.

### 14. COM — ole32.dll (7) + OLEAUT32.dll (2)
`CoInitializeEx`, `CoInitialize`, `CoUninitialize` (apartment init — expected
early), `CoCreateInstance` (creates COM objects, e.g. WASAPI/XAudio2), 
`CoTaskMemAlloc`/`CoTaskMemFree`, `PropVariantClear`; OLEAUT32 `ord#8`/`ord#9`
(BSTR alloc/free). | boot? `CoInitializeEx` **yes**

### 15. Registry — ADVAPI32.dll (7)
`RegOpenKeyExW`, `RegQueryValueExW`, `RegCloseKey` (read game settings /
install path), plus `OpenProcessToken`, `AdjustTokenPrivileges`,
`LookupPrivilegeValueA`, `ImpersonateSelf` (privilege plumbing — can be
no-op'd safely). | boot? likely (settings read)

### 16. Shell / setupapi / rpc — SHELL32, SETUPAPI, RPCRT4
`SHGetKnownFolderPath` (Documents/Saved Games location — save path), the five
`SetupDi*` device-enumeration calls (`SetupDiGetClassDevsW`,
`SetupDiEnumDeviceInterfaces`, `SetupDiGetDeviceInterfaceDetailW`,
`SetupDiGetDeviceInstanceIdW`, `SetupDiDestroyDeviceInfoList` — likely scanning
for a GPU/monitor/audio device), and `RPCRT4!UuidCreate` (GUID for
save/session/telemetry IDs). | boot? maybe (SHGetKnownFolderPath for saves)

### 17. Version resources — VERSION.dll (2)
`GetFileVersionInfoA`, `VerQueryValueA` — reading version resources out of a
file (often GPU-driver/OS version detection). Safe to stub to failure.

### 18. Steamworks — steam_api64.dll (9)
`SteamAPI_Init`, `SteamInternal_ContextInit`,
`SteamInternal_FindOrCreateUserInterface`, `SteamAPI_RegisterCallback`,
`SteamAPI_UnregisterCallback`, `SteamAPI_RunCallbacks`, `SteamAPI_Shutdown`,
`SteamAPI_GetHSteamUser`, `SteamAPI_RestartAppIfNecessary`.
> **Boot-gating.** `SteamAPI_Init` (sometimes `SteamAPI_RestartAppIfNecessary`)
> is commonly called from a static initializer, *before* `main`; failure makes
> the game abort. A working stub already exists in `runtime/src/steam.rs`
> (`SteamAPI_Init` ⇒ 1, `ContextInit` ⇒ 1, `FindOrCreateUserInterface` ⇒ 1)
> but is **not wired into `ImportResolver::call`**. This is a
> high-value/low-effort fix.

### 19. Bink video — bink2w64.dll (14)
`BinkOpen`, `BinkClose`, `BinkNextFrame`, `BinkDoFramePlane`, `BinkWait`,
`BinkPause`, `BinkShouldSkip`, `BinkSetMemory`, `BinkUtilMalloc`,
`BinkUtilFree`, `BinkSetSoundSystem`, `BinkOpenDirectSound`,
`BinkRegisterGPUDataBuffers`, `BinkGetGPUDataBuffersInfo`.
> Plays the publisher/legal logo movies at boot. The engine calls `BinkOpen`
> early; a stub that reports "no video" lets startup continue, a hard miss may
> not. Note the GPU/data-buffer variants: this build feeds Bink frames through
> the graphics layer.

### 20. Sony PS4 conversion layer — libSceJobManager / Gnm / GpuAddress / Pad (48)
* `libSceJobManager.dll` (7): C++ `sce::Job::JobManager` —
  ctor/dtor, `initialize`, `shutdown`, `setPersistentThreadCount`,
  `calculateRequiredMemorySize`, `getSequenceFactoryInterface`. This **is the
  job system's bootstrap**; it runs at engine init and drives the worker pool.
* `libSceGnm.dll` (24): `sce::Gnm::Texture` / `DataFormat` methods —
  `init`, `initAs2d/2dArray/3d/Cubemap`, `build`, getters
  (`getWidth/Height/Depth`, `getTileMode`, `getSizeAlign`, …). Used when
  creating every texture; the renderer cannot come up without them.
* `libSceGpuAddress.dll` (8): `tileSurface`/`detileSurface`,
  `computeTextureSurfaceOffsetAndSize`, `computeSurfaceTileMode`, etc. —
  PS4 tiling maths. Needed if assets are stored tiled; otherwise stub-able.
* `libScePad.dll` (9): `scePadInit`, `scePadOpen`, `scePadRead`,
  `scePadGetControllerType`, `scePadSetVibration[Mode]`,
  `scePadSetLightBar`, `scePadResetLightBar`, `scePadSetParticularMode`.
  Input; the Android layer already has gamepad plumbing.
> `steam.rs::sce()` returns 0 for every one of these and is **not wired** — and
> returning 0 for the textured/`init` methods would be wrong anyway (they return
> handles/status). These need real stubs keyed on the mangled names.

### 21. Networking — WS2_32 / WSOCK32 / WLDAP32 / IPHLPAPI (51)
`WS2_32` (4): `getaddrinfo`, `freeaddrinfo`, `getnameinfo`, `inet_pton`.
`WSOCK32` (30): legacy ordinals (#1…), a 16-bit-era shim over the same stack.
`WLDAP32` (16): LDAP ordinals. `IPHLPAPI!GetAdaptersInfo`: MAC/interface list.
> Matchmaking, telemetry, entitlements. **Not boot-critical** — stubbing to
> "no network" is acceptable to reach `main`; real sockets only matter for
> online play.

### 22. GDI32 (2)
`ExtTextOutA`, `SetBkColor` — text output for the (usually hidden) Win32 window.
Safe to no-op.

---

## Priority to reach the game's `main` (ordered, with reasoning)

The order is: *first make any import dispatch at all, then satisfy the CRT
pre-`main` sequence, then kernel32/COM/registry that static init and WinMain
touch, then windowing/pump, then boot-time subsystems (Steam → Bink → Sony job
→ Sony GNM), last the online/optional pieces.*

**0. Wire the resolver (structural, not a function).**
Call `dispatch::set_import_resolver(Arc::new(resolver))` after feeding
`generated/manifest.json` through `ImportResolver::load_manifest`, in the
`jni.rs` run path. Until then every one of the 507 misses is invisible and RAX
is garbage after each import. Also fix `GetProcAddress` (`resolve_proc` returns
0) and `LoadLibraryA/W` (return 0) if dynamic loading is needed.

**1. CRT startup — `api-ms-win-crt-runtime-l1-1-0` (32).**
`_set_app_type`, `_initterm_e`, `_initterm`, `_configure_narrow_argv`,
`_initialize_narrow_environment`, `_get_narrow_winmain_command_line`,
`_initialize/_register/_execute_onexit_table`, `_crt_atexit`, `exit`, `_cexit`,
`_exit`, `_set_fmode`, `_configthreadlocale`, `_controlfp`,
`_set_invalid_parameter_handler`, `_invalid_parameter_noinfo`,
`_register_thread_local_exe_atexit_callback`, `_getpid`, `_errno`,
`__sys_nerr`, `strerror`, `_beginthreadex`, `_endthreadex`.
*Reason:* this **is** the path from `entry=0x3ec680` to `main`; `_initterm`
runs the C++ static constructors, which is where Steam/COM/subsystems start.

**2. CRT heap — `api-ms-win-crt-heap-l1-1-0` (8).**
`malloc`, `free`, `calloc`, `realloc` (+`_aligned_*`, `_set_new_mode`).
*Reason:* every allocation in the glue and in the game goes here; nothing runs
without a working allocator. Trivial to bind to Bionic.

**3. VCRUNTIME140 mem/str + C++ EH (21, at least mem/str first).**
`memcpy`, `memset`, `memmove`, `memcmp`, `memchr`, `strstr`, `strchr`,
`strrchr`, `wcsstr`, `wcschr`, `wcsrchr`, then `__intrinsic_setjmp`/`longjmp`
and the SEH throw/catch set (`__C_specific_handler`, `__CxxFrameHandler3`,
`_CxxThrowException`).
*Reason:* mem/str are called from CRT startup itself; the EH set is what the
current missing-SEH gap will trip on the first `throw`.

**4. CRT string/ctype/convert/time/env/utility/filesystem/locale (≈105).**
*Reason:* called all over init and gameplay; cheap 1:1 Bionic mappings.

**5. CRT stdio — `api-ms-win-crt-stdio-l1-1-0` (41).**
`__acrt_iob_func`, `_get_osfhandle`, `_fileno`, `fopen[_s]`, `fread`, `fwrite`,
`fseek`/`_fseeki64`, `ftell`/`_ftelli64`, `fclose`, `fflush`.
*Reason:* CRT stream bootstrap plus the game's loose-file/config/log reads.

**6. MSVCP140 in-situ mutex + C++ helpers (10).**
`_Mtx_init_in_situ`, `_Mtx_lock`, `_Mtx_unlock`, `_Mtx_destroy_in_situ`,
`_Throw_C_error`, `_Xlength_error`. *Reason:* static-init-time `std::mutex`.

**7. KERNEL32 core needed by init (subset of 84).**
`GetStartupInfoW`, `SetLastError`, `GetStdHandle`, `GetFileType`, `ExitProcess`,
`InitializeCriticalSectionEx`, `TryEnterCriticalSection`, `InitializeSListHead`,
`SetThreadExecutionState`, `GetSystemDirectoryW`, `GetFullPathNameW`,
`WideCharToMultiByte`, `MultiByteToWideChar`, `FormatMessageA/W`,
`RaiseException`, `DebugBreak`, `GlobalMemoryStatusEx`,
`K32GetProcessMemoryInfo`, `LocalAlloc`/`LocalFree`/`LocalSize`, and the `Rtl*`
unwinder trio. *Reason:* CRT startup + engine probing.

**8. COM — ole32 (7).** `CoInitializeEx`/`CoInitialize`, `CoUninitialize`,
`CoTaskMemAlloc/Free`, `PropVariantClear`, `CoCreateInstance`.
*Reason:* COM apartment must be up before any COM-based subsystem (audio,
XAudio2-style objects, DXGI helpers) is created.

**9. Registry + shell + version — ADVAPI32 (7), SHELL32 (1), VERSION (2).**
`RegOpenKeyExW`/`RegQueryValueExW`/`RegCloseKey`, `SHGetKnownFolderPath`,
`GetFileVersionInfoA`/`VerQueryValueA` (+`RPCRT4!UuidCreate`).
*Reason:* settings/install-path/save-folder and version probes during init.

**10. USER32 windowing + message pump (59).**
`RegisterClassExA`, `CreateWindowExA`, `ShowWindow`, `AdjustWindowRect`,
`GetClientRect`, `PeekMessageA`, `TranslateMessage`, `DispatchMessageA`,
`DefWindowProcA`, `PostQuitMessage`, the `*WindowLongPtr*` set, `LoadCursorA`,
`LoadIconA`.
*Reason:* a window and a message pump must exist before the render loop; can be
backed by the existing Android `SurfaceView`/ANativeWindow.

**11. Steamworks — steam_api64 (9).**
*Reason:* DRM/init gate that often runs in a static initializer; the stub
already exists in `steam.rs` — just route it. Highest value-per-line item.

**12. Bink video — bink2w64 (14).**
*Reason:* boot logo/cutscene playback right after startup; a "no video" stub
should let boot proceed.

**13. Sony job system — libSceJobManager (7).**
*Reason:* engine worker-pool bootstrap; needed for the engine to finish init.

**14. Sony GNM + GPU address — libSceGnm (24) + libSceGpuAddress (8).**
*Reason:* texture creation/tiling; needed with the renderer, after the job
system. Requires real per-method stubs (not blanket 0).

**15. Sony pad — libScePad (9).** Input; the Android pad layer can back it.

**16. Networking — WS2_32/WSOCK32/WLDAP32/IPHLPAPI (51).**
*Reason:* online/telemetry only; **last** and safe to stub as "offline".

---

## Honest summary

* 579 imports across 38 DLLs; **72 handled, 507 missing (12.4 %)**.
* Of the 72 "handled", 3 are graphics bootstraps that only work with a backend
  `.so` loaded, and several kernels32 handlers are documented stubs
  (`GetProcAddress`→0, `GetLastError`→0, `LoadLibrary*`→0,
  `WaitForMultipleObjects`→fake-signal).
* The runtime is **not currently wired to the import table at all**
  (`set_import_resolver` has no caller) — the true first blocker.
* The pre-`main` path is essentially unserved: none of the CRT
  `api-ms-win-crt-*`, `VCRUNTIME140`, or `MSVCP140` imports is implemented.
* `docs/STATUS.md`'s `bcrypt`/`crypt32` claim is not supported by the IAT;
  networking is actually `WSOCK32`/`WLDAP32`/`WS2_32`/`IPHLPAPI`.
* `steam.rs` and the XAudio2 support exist but are dormant for this EXE.
