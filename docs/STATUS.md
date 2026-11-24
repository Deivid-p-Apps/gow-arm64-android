# STATUS

## Fase atual: M1 em andamento — runtime host executa código recompilado; APK no CI

### O que é este projeto
Recompilação estática (AOT) do executável x86-64 do God of War (2018) PC para
aarch64 (Android), em Rust. Sem JIT, sem Wine, sem emulador em tempo de execução.
O código do jogo é traduzido para C em tempo de build, compilado para arm64 e roda nativo.

### Alvo (medido no GoW.exe real)
- PE32+ x86-64, image_base `0x140000000`, entry `0x3ec680`
- `.text` = ~13.9 MB; **51.869 funções** na tabela `.pdata`; TLS callbacks presentes
- 7 seções, 579 imports, 61 exports
- Imports-chave: `d3d11`, `dxgi`, `D3DCOMPILER_47`, `xinput1_4`, `steam_api64`,
  `bink2w64`, `kernel32`, `user32`, `advapi32`, `ws2_32` (ordinals), `wldap32`,
  `iphlpapi`, `rpcrt4`, `ole32`, `setupapi`, `shell32`, CRTs (`api-ms-win-crt-*`,
  `vcruntime140`, `msvcp140`). Camada Sony: `libSceGnm/Pad/JobManager/GpuAddress`.

### Dispositivo de teste (ADB)
Motorola Edge 30 Fusion (`tundra`), SM8350 (Snapdragon 888+), Adreno 725 (Turnip),
~7.5 GB RAM, Android 14, 1080x2400 @ até 144 Hz.

### O que funciona (verificável)
- [x] `tools/pe_probe.py`: análise de PE
- [x] `recompiler`: parser PE32+ (sections/IAT/exports/.pdata/TLS)
- [x] `recompiler`: descoberta de funções (`.pdata` + entry + TLS callbacks +
      varredura de `call`/`jmp` diretos em `.text`) → **53.787 funções**
- [x] `recompiler`: lifter x86-64 -> C (iced-x86)
- [x] **Modelo de fluxo de controle correto: chamadas C diretas + `goto` local**
      (provado end-to-end: um programa sintético de 2 funções roda e dá `rax=15`)
- [x] **Lift completo: 53.787 funções, 2.960.366 instruções; cobertura 99,99%**
      (288 traps). 36 shards `gow_shard_NNN.c` + `gow_table.c` + `gow_protos.h`
- [x] **Execução real no dispositivo**: o aparelho carrega `libgow_game.so`,
      registra as 53.787 funções, instala o resolver de imports (579 slots),
      mapeia a imagem do PE em `0x140000000` (mmap MAP_FIXED) e **entra no entry
      point `0x3ec680`**, executando código recompilado de verdade
      (backtrace real: `entry f_3ec680 → f_3ed174 → gow_load`)
- [x] **C gerado compila de verdade** com `clang -O2` (shards testados, 0 erros)
- [x] `runtime`: dispatch loop, arquivo de registradores, memória guest no image
      base original, pilha guest (8 MB), flags exatas, shifts, SSE/SSE2/AVX subset,
      MMX aliasing, conversões, comparações, string ops, pack/pshufb/pminmax/etc.
- [x] `runtime`: camada Win32/NT (import resolver por IAT) com kernel32 (memória,
      threads, sync, timers) e **I/O de arquivos mapeado para o game dir**
- [x] `runtime`: stubs Steamworks / Sony / XInput
- [x] `runtime`/`jni.rs`: carrega o `.so` do jogo (do usuário), registra a tabela, roda
- [x] `android/`: Gradle, MANIFEST com MANAGE_EXTERNAL_STORAGE, SurfaceView,
      loader de driver Turnip/adrenotools em C (JNI), logcat bridge
- [x] CI: build do recompilador + teste end-to-end de fluxo + APK debug como artifact
- [x] **APK instalado e executado no dispositivo via ADB sem crash; lê /storage/emulated/0/god**
- [x] Testes: semântica de flags/cc + fluxo de controle ponta-a-ponta

### O que falta
- [ ] Completar ~95 mnemônicos restantes (x87, SSE antigos raros, port I/O, syscall)
- [ ] Comportamento Win32 além do núcleo (threads reais, sincronização, timers mapeados)
- [ ] D3D11 -> Vulkan (DXVK Native fase 1; backend nativo em Rust fase 2)
- [ ] Áudio (XAudio2 -> AAudio/Oboe), entrada (XInput -> Android), swapchain (ANativeWindow)
- [ ] Linkar todos os 35 shards num `.so` e carregar o jogo real no dispositivo
- [ ] Medição de FPS real nos 4 cenários

## Bugs conhecidos
- 224 instruções viram `gow_unimpl` (registradas, nunca silenciosas): `Vpackssdw`
  (forma YMM de 256 bits), I/O de porta (`in`/`out`/`insb`/`outsd`),
  `iret`/`syscall`/`sysret`/`lar`/`lsl`, `fbld`/`fbstp` (BCD 80-bit).
- Jump tables ambíguas (jmp [table+idx*8]) precisam do mapa de ponteiros resolvido.
- SEH (exceções x64) ainda não modelado.

## Bugs críticos corrigidos (com teste de regressão)
- **Condition codes off-by-one**: iced usa O=1..G=16; gow_test_cc espera O=0..G=15.
  Sem o `-1` em `cc_const`, TODO salto condicional testava a condição errada.
- **mem_size() errado**: dividia por 8, mas `iced::MemorySize::size()` já é em bytes.
  Resultado: `mov qword [mem], reg` gravava 1 byte; todos os acessos de memória
  estavam com a largura errada. Corrigido + detecção de float x87 (`==4`, não `==32`).

## Próximos passos
1. Fechar a cobertura do lifter (69 mnemônicos).
2. Camada Win32 de arquivos/threads/sincronização sobre Bionic.
3. Começar o bridge D3D11->Vulkan e a swapchain.
