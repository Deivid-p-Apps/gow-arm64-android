# STATUS

## Fase atual: M0/M1 — análise do executável e recompilador estático funcional

### O que é este projeto
Recompilação estática (AOT) do executável x86-64 do God of War (2018) PC para
aarch64 (Android), em Rust. Sem JIT, sem Wine, sem emulador em tempo de execução.
O código do jogo é traduzido para C em tempo de build, compilado para arm64 e roda nativo.

### Alvo (medido)
- Binário: `GoW.exe`, PE32+ x86-64, image_base `0x140000000`, entry `0x3ec680`
- `.text` = 0x00d46600 (~13.9 MB), **51.869 funções** na tabela `.pdata`
- Imports: `d3d11.dll`, `dxgi.dll`, `D3DCOMPILER_47.dll`, `dinput8`, `xinput1_4`,
  `steam_api64.dll`, `bink2w64.dll`, `kernel32`, `user32`, `advapi32`, `ws2_32`,
  `bcrypt`, `crypt32`, `ole32`, `setupapi`, `rpcrt4`, e o CRT (api-ms-win-crt-*)
- Exports: 61. TLS callbacks: presentes.
- APIs Sony (`libSceGnm`, `libScePad`, `libSceJobManager`, `libSceGpuAddress`) aparecem
  nos imports desta cópia — é a build de PC com camada de compatibilidade; ver NOTES.md.

### Dispositivo de teste (ADB)
Motorola Edge 30 Fusion (`tundra`), SM8350 (Snapdragon 888+), Adreno 725 (Turnip disponível),
~7.5 GB RAM, Android 14, tela 1080x2400 até 144 Hz.

### O que funciona hoje (verificável)
- [x] `tools/pe_probe.py`: análise de PE (headers, seções, imports, exports, .pdata, TLS)
- [x] `recompiler`: parser PE32+ completo (sections/imports/exports/IAT/pdata/TLS)
- [x] `recompiler`: descoberta de funções por `.pdata` + entry + TLS callbacks
- [x] `recompiler`: lifter x86-64 -> C movido por iced-x86
- [x] Execução real: 51.869 funções, 2.756.013 instruções geradas em `generated/gow_game.c`
- [x] `runtime`: dispatcher (`gow_call`/`gow_call_indirect`/`gow_ret`), arquivo de registradores,
      memória guest mapeada no image base original, gow_setflags, gow_load/store
- [x] Relatório de cobertura de mnemonics (falha alto, nunca silencioso)

### O que falta (grande)
- [ ] Lifter: cobertura de 292 mnemônicos ainda não implementados (SSE/SSE2+, cmov, bit ops,
      adc/sbb, string ops, x87, AVX) — ver `lift.log`
- [ ] Ordenação de memória x86 (TSO) vs ARM: usar ld.acquire/st.release para memória compartilhada
- [ ] Camada Win32/NT (kernel32/ntdll/user32/... -> Bionic)
- [ ] D3D11 -> Vulkan (DXVK Native no APK, fase 1; backend Vulkan nativo em Rust, fase 2)
- [ ] Áudio (XAudio2 -> AAudio/Oboe), entrada (XInput -> Android), janela (ANativeWindow)
- [ ] Empacotamento do APK e loop CI -> artifact -> `adb install` -> medição

## Bugs conhecidos
- `gow_setflags` ainda não calcula CF/OF corretamente para adc/sbb/inc/dec (placeholder conservador).
- `gow_jump`/`gow_ret` ainda não retomam o fluxo de controle real (faltam setjmp/estado de PC).
- Funções regulares dentro de `.pdata` OK; jump tables relativas a RIP não roteadas ainda.

## Próximos passos
1. Completar lift de SSE/SSE2 (a maior parte do código vetorizado do jogo) e cmov.
2. Introduzir máquina de estados por bloco básico com função de retomada (para call/ret corretos).
3. Começar a camada Win32 (kernel32 mínimo: VirtualAlloc, CreateThread, sync primitives).
4. CI: workflow que compila o recompilador e roda contra um binário de teste sintético.
