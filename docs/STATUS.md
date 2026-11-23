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
  `bink2w64`, `kernel32`, `user32`, `advapi32`, `ws2_32`, `bcrypt`, `crypt32`,
  `ole32`, `setupapi`, `rpcrt4`, CRTs. Camada Sony: `libSceGnm/Pad/JobManager/GpuAddress`.

### Dispositivo de teste (ADB)
Motorola Edge 30 Fusion (`tundra`), SM8350 (Snapdragon 888+), Adreno 725 (Turnip),
~7.5 GB RAM, Android 14, 1080x2400 @ até 144 Hz.

### O que funciona (verificável)
- [x] `tools/pe_probe.py`: análise de PE
- [x] `recompiler`: parser PE32+ (sections/IAT/exports/.pdata/TLS)
- [x] `recompiler`: descoberta de funções (`.pdata` + entry + TLS callbacks)
- [x] `recompiler`: lifter x86-64 -> C (iced-x86)
- [x] **Lift completo: 51.869 funções, 2.750.564 instruções; cobertura 99,97%**
      (867 traps restantes). Gera 35 shards `gow_shard_NNN.c` + `gow_table.c`
- [x] **C gerado compila de verdade** com `clang -O2` (shards testados, 0 erros)
- [x] `runtime`: dispatch loop exato (call/ret/jmp/jcc), arquivo de registradores,
      memória guest no image base original, flags exatas (add/sub/logic), shifts,
      SSE/SSE2/AVX subset, MMX aliasing, conversões, comparações, string ops
- [x] `runtime`: camada Win32/NT (resolver de imports por IAT; kernel32 core) +
      stubs Steamworks / Sony / XInput
- [x] `runtime`/`jni.rs`: carrega o `.so` do jogo (do usuário), registra a tabela, roda
- [x] `android/`: projeto Gradle, MANIFEST com MANAGE_EXTERNAL_STORAGE, SurfaceView,
      loader de driver Turnip/adrenotools em C (JNI)
- [x] CI: build do recompilador + APK debug como artifact
- [x] Testes: semântica de flags e condition codes

### O que falta
- [ ] Completar os 69 mnemônicos restantes (SSE antigos: unpck/psll/psrl/pshufb/haddps/
      pextrw/insertps/roundps/blendps; x87; port I/O)
- [ ] Comportamento da camada Win32 além do núcleo kernel32 (arquivos mapeados para
      /storage/emulated/0/god, threads, sincronização, timers)
- [ ] D3D11 -> Vulkan (DXVK Native numa fase 1; backend nativo em Rust na fase 2)
- [ ] Áudio (XAudio2 -> AAudio/Oboe), entrada (XInput -> Android), swapchain (ANativeWindow)
- [ ] Baixar artifact do CI e testar no dispositivo via ADB (install/logcat/medição)

## Bugs conhecidos
- 867 instruções viram `gow_unimpl` (registradas, nunca silenciosas).
- Jump tables ambíguas (jmp [table+idx*8]) precisam do mapa de ponteiros resolvido.
- Threads/SHE (SEH) ainda não modelados.

## Próximos passos
1. Fechar a cobertura do lifter (69 mnemônicos).
2. Camada Win32 de arquivos/threads/sincronização sobre Bionic.
3. Começar o bridge D3D11->Vulkan e a swapchain.
