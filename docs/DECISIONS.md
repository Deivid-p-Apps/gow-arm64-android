# DECISIONS

Decisões de engenharia e o porquê. Mais recentes no topo.

## D001 — Linguagem: Rust para recompilador e runtime
Rust para o recompilador e o runtime (dispatcher, memória, camadas Win32 e Vulkan).
C apenas para glue inevitável (DXVK Native na fase 1, libadrenotools). Alinhado ao prompt.

## D002 — Estratégia de lifting: x86-64 -> C, compilado por clang para arm64
Escolhido emitir C (não LLVM IR direto) porque: (a) o clang otimiza o C para arm64 com
NEON e barreiras corretas; (b) é o mesmo caminho de projetos x86 reais (recomp-kit,
x64-mac-recomp, Skyrim_Android); (c) depuração é direta. LLVM/inkwell fica como opção futura.

## D003 — Descoberta de funções por `.pdata`
O binário traz tabela de exceção x64 com 51.869 entradas — enumera todas as funções
sem heurística. Usada como raiz de funções, somada a entry point e TLS callbacks.

## D004 — Casamento de ponteiros guest: mapear no image base original
Em vez de traduzir todos os ponteiros, mapeamos a imagem guest no endereço `0x140000000`
(mm novo no runtime; mmap MAP_FIXED em Android/Linux). Assim ponteiro guest == ponteiro
host e não há marshalling. Mesma abordagem do x64-mac-recomp (VA unificado acima do carveout).

## D005 — Insucesso explícito, nunca silencioso
Instruções não liftadas viram `gow_unimpl(rva, mnemonic)` e contam/registram. Coisas como
"fim do jogo" jamais podem ser fingidas. O relatório de cobertura é parte do output do build.

## D006 — Fluxo de build: GitHub Actions só compila; teste é local via ADB
Conforme o prompt: o workflow gera o APK como artifact; o download e o teste
(install/logcat/screencap/medição) acontecem no Termux com o ADB ligado ao aparelho.
O repositório não guarda arquivos do jogo (nem código gerado a partir deles).

## D007 — Reprodutibilidade: código gerado nunca entra no repo
`generated/` é ignorado. O repositório contém só ferramentas e runtime; o usuário
fornece sua cópia do jogo e gera o código localmente.

## D008 — Modelo de fluxo de controle: chamadas C diretas, não trampolim
Recompiladores que roteiam `call`/`ret` por um trampolim central de endereços
quebram porque endereços de retorno caem no meio de funções (não são entradas
válidas). Decisão: chamadas inter-função viram chamadas C diretas (`f_<rva>(cpu)`);
ramos intra-função viram `goto L_<rva>`; apenas alvos computados/desconhecidos vão
para `gow_trampoline`. Provado ponta-a-ponta com um programa sintético (rax=15).

## D009 — x86-64 -> C (não LLVM IR direto)
Mantido de D002. O clang (inclusive o do Termux, que mira
`aarch64-unknown-linux-android24`) otimiza o C para arm64 com NEON e barreiras.
O `.so` do jogo gerado aqui já é arm64 Android real.

## D010 — Sharding do código gerado
O C do jogo tem ~295 MB. Compilar num só TU estoura a RAM (aparelhos com ~1.5 GB
livres) e o próprio CI. Decisão: 1.500 funções por shard (~35 shards de ~3.6 MB),
cada um compilável isolado; protótipos compartilhados em `gow_protos.h`.

## D011 — DXVK Native na fase 1, backend Rust nativo na fase 2
Conforme o prompt: colocar o jogo de pé com DXVK Native (d3d11+dxgi -> Vulkan),
depois substituir por um backend Vulkan nativo em Rust (ash) com DXBC->SPIR-V.
O backend nativo é o alvo final; DXVK é fallback até a paridade.

## D012 — Falha explícita em cada camada
Imports desconhecidos, alvos de salto não resolvidos e instruções não liftadas são
contados e logados (logcat), nunca silenciados. Nenhuma métrica é fabricada;
`docs/PERFORMANCE.md` só recebe números medidos no dispositivo.

## D020 — Chunks .pdata contíguos com fall-through são UMA função
O MSVC emite UMA função lógica como VÁRIOS registros `.pdata` contíguos que caem
um no outro (ex.: o helper de `std::string` `0xb3810` termina no meio da lógica em
`0xb386b`, que continua até `0xb38d9`). O lifter tratava cada registro como uma
função completa e fechava a função C sem transferir para o próximo chunk, então o
chunk retornava cedo com os registradores guest clobberados — em particular `rbp`
ficava com `[string+0x18] = 0xf` (a capacidade SSO do `std::string` do MSVC).
O chamador (`f_4024e0`) usa `rbp` como base de frame, então o próximo acesso
`movdqu [rbp-0x38]` calculava `0xffffffffffffffd7` (exatamente o primeiro
`badstore` do log), destruía o frame e fazia `__security_check_cookie` falhar →
`TerminateProcess(0xC0000409)` (STATUS_STACK_BUFFER_OVERRUN) → loop degradado.
Havia 18.569 chunks assim em 51.869 do `.pdata`.

Correção: `func_end()` agora **segue a cadeia de fall-through** (chunk sem padding
final, última instrução real não-terminadora, e o próximo chunk começa exatamente
onde este termina) e o emissor decodifica a cadeia inteira como uma função só,
com os rótulos/gotos cruzando os limites. O fallback de fall-through simples
(`f_<range_end>(cpu); return;`) fica como rede de segurança.

## D021 — Log de fault de memória com dedup por (rva, addr)
Um guest corrompido em loop apertado gerava 277.027 linhas `badload` idênticas
(455 MB de log), enterrando a causa real e enchendo o armazenamento do aparelho.
Cada par (rva, addr) agora loga uma vez só, com teto de 4096 entradas.

## D022 — `call qword [mem]` carrega o PONTEIRO do slot (não o endereço)
O lifter emitia `gow_call_ptr(cpu, mem_expr(...))` para `call qword [rbx+0x10]`,
passando o ENDEREÇO do slot (`rbx+0x10`) como se fosse a função, em vez de ler o
ponteiro guardado nele. Isso quebrou TODA chamada virtual/objeto-relative — as
vtables do engine — e produzia os `[gow.MISSCALL] indirect target_va=0xb400...10`
(na verdade `param+0x10`). Eram **22.498** sites no binário. Agora usa
`operand_read`, que emite `gow_load(cpu, rbx+0x10, 8)` — o valor correto.

## D023 — Template TLS do módulo é copiado para o bloco TLS de cada thread
O loader do Windows copia `[StartAddressOfRawData, EndAddressOfRawData)` do
diretório TLS para o bloco TLS de cada thread no início dela. O runtime alocava
os 128 slots TLS zerados mas nunca copiava o template, então variáveis
`thread_local` do engine liam 0 e o código dereferenciava NULL (log do device:
store em `rcx=0` no rva 0x400cbb, via `gs:[0x58]` -> `[*tlsp]` -> `[[tls]+0x28]`).
`pe_load` agora registra o template e `GowCpu::new` o copia para o slot 0.

## D024 — Chunks: `call [mem]` + stubs CRT + dedup de log
Além de D020/D022/D023: `getenv`/`wcstombs`/`mbstowcs` implementados (apareciam
como "unimplemented" no boot); log de `badload`/`badstore` com dedup por
`(rva, addr)` (um loop corrompido gerava 455 MB de log e enterrava a causa raiz).

## D025 — Diagnóstico do hang: MISSFN + trace no sampler + template TLS do CPU main
O loop de dispatch incrementava `unimpl` quando um rva não tinha entrada liftada,
mas NÃO logava — então "unimpl=2495" não dizia qual função faltava. Agora loga
`[gow.MISSFN]` uma vez por rva. O sampler também imprime o anel de chamadas
(`[gow.sample.trace]`) do CPU principal. E `refresh_tls_template` copia o template
TLS para o bloco do CPU principal, que é criado ANTES do `load_into` (o template
só era aplicado nos threads novos).

## D026 — Descoberta de funções por ponteiros de código em .rdata/.data (vtables)
O descobridor só seguia `call/jmp rel32` direto. Mas o engine guarda ponteiros de
função em tabelas (vtables, tabelas de handlers/jump) nas seções de dados e as
chama indiretamente. Isso deixava 512 funções em `.text` SEM lift, e o loop de
dispatch caía em `[gow.MISSFN] ... no lifted entry` (build #171, thread principal
travado). Windows x64 garante funções alinhadas a 16 bytes; agora toda seção NÃO
executável é varrida por valores de 8 bytes que apontam para um RVA alinhado a 16
dentro de uma seção executável, e esses RVAs viram entradas. Resultado: 96.324
funções (de 90.943), MISSFN caiu de 512 → 1 (o único restante é `0xd4e660`, um
ponteiro em `.rdata` que não é código, e o dispatch o ignora).

## D027 — Shell/disco/console: o boot para de mentir "Sem espaço/permissão"
Com o D026 o jogo passou a chegar MUITO mais longe: boot real até `SHGetKnown
FolderPath`, `GetDiskFreeSpaceExW` e um `MessageBox` de erro. Faltavam:
- `SHGetKnownFolderPath`: cria "SavedGames"/"Documents" sob o game dir e devolve o
  caminho em wide-string alocada (antes: unimplemented → "Permissions Error:
  Unable to access Saved Games" em loop).
- `GetDiskFreeSpaceExW`/`W`: `statvfs` do game dir (antes: unimplemented → 0 →
  "Not Enough Disk Space" em loop).
- `GetDiskFreeSpaceW`, `setvbuf`, `LocalAlloc/Free`, `CoTaskMemAlloc/Free`,
  `GetConsoleWindow`/`ShowWindow`/`DestroyWindow`/`FreeConsole`.

## D028 — DXVK 3.1.1 rejeita o Turnip por `storageBuffer8BitAccess=0` (relaxado, estilo Sarek)
Diagnóstico no aparelho (sonda que usa o MESMO `vkGetInstanceProcAddr` entregue ao
DXVK): o driver é `Turnip Adreno (TM) 660 api=1.3.363`, ou seja É Vulkan 1.3 — mas
`vkGetPhysicalDeviceFeatures2` retorna `storageBuffer8BitAccess=0`. O DXVK 3.1.1
tem essa feature como **obrigatória** (`ENABLE_FEATURE(vk12, storageBuffer8Bit
Access, true)`), então rejeita o adapter ("Skipping: Device does not support
required feature") → `No adapters found` → `CreateDXGIFactory1 -> 0x80004005`.

A comunidade resolve isso com o **DXVK-Sarek** (pythonlover02/DXVK-Sarek), que
marca exatamente essas exigências como opcionais em GPUs mobile. O patch
`tools/patches/0006` faz o mesmo no DXVK 3.1.1:
- `GOW_RELAX(require)` torna todas as features/extensões obrigatórias **opcionais**
  (as que o driver suporta continuam sendo habilitadas; só o portão de boot sai);
- o gate de apiVersion cai para 1.1;
- o gate de 256 bytes de push-constant (Turnip expõe 128) é relaxado.
Assim uma feature ausente vira glitch de render, não falha de inicialização.

## D029 — DXVK-Native continua o backend padrão? Não: backend NATIVO em paralelo
Além do DXVK (patch 0006), o projeto ganha um backend Vulkan **nativo em Rust**
(sem tradução D3D11), selecionável por env var, para não depender do DXVK. Os dois
coexistem; a seleção é por `GOW_GFX_BACKEND` (default `dxvk`).

## D030 — Chamadas COM virtuais despachadas para código host (DXVK)
Com o D029/patch 0006 o DXVK aceitou o Turnip e `CreateDXGIFactory1` retornou
`S_OK`. O passo seguinte falhou: o jogo chama os MÉTODOS VIRTUAIS da factory
(`CreateSwapChain`, `GetAdapter`, ...), cujo código vive DENTRO do
`libdxvk_dxgi.so`. O `gow_call_indirect` tratava esses endereços host como
MISSCALL. Agora:
- `hostcall` mantém um registro de faixas de código host chamável;
- `graphics` registra, no dlopen de cada backend, as faixas executáveis lidas de
  `/proc/self/maps`;
- `gow_call_indirect`, se o alvo cai numa faixa registrada, faz o marshaling dos
  registradores MS-x64 do guest para a chamada host e guarda o retorno em RAX
  (o mesmo caminho já usado para imports da IAT).

## D031 — Pool de VRAM do engine além do bloco: páginas mapeadas sob demanda + bug de registrador
Comparando com o DXVK (que também trava, mas NUNCA falha nesse ponto), achei a causa
do aborto em `f_40a610`: o gerenciador de VRAM do engine monta uma free-list andando
em chunks de 0x10000 e acessa alguns GiB ALÉM do bloco que recebeu; nosso heap
mmap não tinha nada mapeado lá, então o store dava SIGSEGV e abortava o boot.

Correções:
- `gow_load`/`gow_store`: quando a página pertence (ou está a ≤8 GiB de) uma reserva
  nossa, fazem `mmap(MAP_FIXED)` dessa única página e repetem — igual ao Windows, onde
  um reserve de `VirtualAlloc` é VA válido. Nunca mapeia dentro da imagem do jogo.
- Bug real: `kernel32!malloc` compartilhava o handler de `HeapAlloc` e lia o tamanho
  de R8 em vez de RCX, então um `malloc(32 MiB)` devolvia um bloco de tamanho errado.
- `VirtualAlloc` ganha um pequeno slack e alinhamento de 64 KiB (granularidade).

Resultado (host runner, Turnip): o jogo REAL passa do `f_40a610`, roda o `_initterm`
completo e entra no loop de jobs com as 6 threads de trabalho — sem abortar
(exit=timeout, não SIGSEGV). Antes: SIGSEGV antes de qualquer thread.
