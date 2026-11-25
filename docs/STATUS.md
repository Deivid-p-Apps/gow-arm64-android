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
- [x] `runtime`: **heap Win32 rastreado** (D013): alocações registradas por
      ptr -> tamanho/alinhamento; `free`/`HeapFree`/`HeapReAlloc`/`VirtualFree`
      devolvem memória de verdade; teto `GOW_HEAP_MB` (default 3072 MiB) com
      falha explícita em vez de OOM-kill; heap reporta crescimento em bandas de
      128 MiB e o sampler de 2 s exibe o RSS do processo; blocos >= 32 MiB saem
      de mmap(MAP_NORESERVE) — commit lazy estilo Windows, teto separado
      `GOW_RESERVE_MB` — para a arena multi-GiB do motor (D017)
- [x] `runtime`: **`LoadLibrary`/`GetProcAddress` dinâmicos** (D014):
      pseudo-módulos para qualquer DLL e thunks hostcall que empacotam os
      argumentos MS x64 — `D3D11CreateDevice`/`CreateDXGIFactory1` resolvidos
      por nome chamam o DXVK Native carregado, sem passar pelo IAT
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
- [x] D3D11 -> Vulkan **fase 1**: DXVK Native v3.1.1 para Android aarch64 compila no
  CI (job `dxvk`), vai para o release `dxvk-latest` e entra no APK em
  `lib/arm64-v8a/libdxvk_{d3d11,dxgi}.so`; o runtime carrega ambos, seta
  `DXVK_WSI_DRIVER=Android` e empurra o `ANativeWindow` para os dois. Receita
  verificada em `docs/DXVK_ANDROID.md` §10 (run verde 45410666526).
- [x] Driver Adreno customizado via **libadrenotools** (BSD-2, job `adrenotools`
  do CI, release `adrenotools-latest`): o runtime carrega `libadrenotools.so`,
  chama `adrenotools_open_libvulkan` com o Turnip escolhido no launcher, e passa
  o `vkGetInstanceProcAddr` desse loader tanto para `vk.rs` quanto para os dois
  DXVK (`tools/patches/0005`, símbolo `dxvk_vulkan_set_get_instance_proc_addr`). Sem driver =
  driver do sistema, nunca crasha. Run verde **45473778373**. Ver
  `docs/GRAPHICS.md` §"Custom driver".
- [ ] Backend nativo em Rust (fase 2) e confirmar o primeiro frame em tela
  (`D3DReflect` via `runtime/src/dxbc.rs` e o caminho de formato da swapchain
  ainda não testados contra o jogo real)
- [ ] Áudio (XAudio2 -> AAudio/Oboe), entrada (XInput -> Android), swapchain (ANativeWindow)
- [ ] Linkar todos os 35 shards num `.so` e carregar o jogo real no dispositivo
- [ ] Medição de FPS real nos 4 cenários

## Bugs conhecidos
- 224 instruções viram `gow_unimpl` (registradas, nunca silenciosas): `Vpackssdw`
  (forma YMM de 256 bits), I/O de porta (`in`/`out`/`insb`/`outsd`),
  `iret`/`syscall`/`sysret`/`lar`/`lsl`, `fbld`/`fbstp` (BCD 80-bit).
- Jump tables ambíguas (jmp [table+idx*8]) precisam do mapa de ponteiros resolvido.
- SEH (exceções x64) ainda não modelado.
- `WaitForMultipleObjects` retorna sempre WAIT_OBJECT_0 (o primeiro objeto):
  suficiente para o boot, mas um wait multi-objeto real pode girar em loop.

## Bugs críticos corrigidos (com teste de regressão)
- **Condition codes off-by-one**: iced usa O=1..G=16; gow_test_cc espera O=0..G=15.
  Sem o `-1` em `cc_const`, TODO salto condicional testava a condição errada.
- **mem_size() errado**: dividia por 8, mas `iced::MemorySize::size()` já é em bytes.
  Resultado: `mov qword [mem], reg` gravava 1 byte; todos os acessos de memória
  estavam com a largura errada. Corrigido + detecção de float x87 (`==4`, não `==32`).
- **Heap que nunca liberava + arredondamento a 4 KiB** (D013): `host_free` era
  no-op e toda alocação virava uma página inteira — o app consumia toda a RAM
  até o LMK do Android matar (o "consome toda a RAM e fecha" dos logs).
  Corrigido com tabela rastreada (free/realloc/VirtualFree reais, teto
  `GOW_HEAP_MB`) e log de crescimento em bandas + RSS no sampler.
  `HeapReAlloc` copiava além do bloco antigo (overread) — agora copia
  min(antigo, novo).
- **GetProcAddress sempre retornava 0** (D014): o jogo resolvia
  `D3D11CreateDevice` dinamicamente, guardava NULL e chamava — a tempestade
  `MISSCALL target_va=0x0` dos logs, e o device D3D11 nunca era criado (nada
  renderizava). Agora LoadLibrary/GetProcAddress servem thunks hostcall
  (DXVK Native para d3d11/dxgi/d3dcompiler).
- **Args MS x64 errados**: `CreateThread` com start/param trocados (start é R8);
  5º argumento lido em `arg(4)` em vez de `arg(0)` (disposition do CreateFile,
  size_lo do CreateFileMapping, count do MapViewOfFile); whence do
  SetFilePointer/Ex lido do slot errado; strings `*W` (UTF-16) lidas como ANSI
  porque o teste de largura usava o nome da DLL, não o sufixo da função.
- **Imagem guest remapeada por thread** (D015): cada `GowCpu::new` fazia
  MAP_FIXED sobre o mesmo endereço — a primeira thread criada zeraria o código
  que a main executava. Agora o mapeamento é compartilhado por base num
  registro global; a CPU do sampler não é mais liberada sob os pés da thread
  de diagnóstico; MISSCALL tem dedup por alvo distinto.
- **MapViewOfFile copiava o arquivo inteiro para a RAM** (D016): packs de GBs
  iam integralmente para memória e nunca saíam. Agora mmap MAP_PRIVATE +
  UnmapViewOfFile real (munmap), com fallback para o caminho antigo.
- **Arena multi-GiB recusada pelo teto do heap** (D017): o motor reserva uma
  arena enorme no boot (uma única alocação >= 3 GiB — o "budget exceeded:
  0 MiB live" do log, com o heap ainda vazio). Com NULL em mãos, o boot
  prosseguiu a partir de um heap nulo: cascata de `[gow.badstore]`/
  `[gow.badload]` em endereços negativos e SIGSEGV fatal em 0x80 dentro do
  runtime ao ler um nome de módulo ("KERNEL32.dll" ainda quente nos
  registradores x6/x7). Agora blocos >= 32 MiB saem de mmap(MAP_NORESERVE):
  o espaço de endereços é reservado, páginas zeradas chegam no primeiro toque
  (commit lazy como no Windows), o RSS cresce só com o que o jogo usa, e um
  teto separado `GOW_RESERVE_MB` (default 8192) se aplica; o log de recusa
  agora inclui o tamanho pedido. No mesmo commit: `GetThreadId` e
  `DuplicateHandle` implementados (o CRT chama os dois no boot);
  `GetCurrentThreadId` é estável por thread (o contador antigo devolvia um id
  NOVO a cada chamada, crescendo caches por-thread sem limite);
  `GetModuleHandleEx*` trata GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS (RDX é
  endereço, não string); `VirtualAlloc` com dwSize == 0 sobre uma reserva
  existente devolve a base (commit sobre reserva); `read_cstr` rejeita
  ponteiros selvagens (página nula / faixa kernel) em vez de derrubar o
  processo; MISSCALL agora loga o RVA do ponto de chamada.

## Próximos passos
1. Fechar a cobertura do lifter (69 mnemônicos).
2. Camada Win32 de arquivos/threads/sincronização sobre Bionic.
3. Começar o bridge D3D11->Vulkan e a swapchain.
