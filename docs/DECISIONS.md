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

## D013 — Heap Win32 rastreado: free de verdade, sem arredondar tudo a 4 KiB
A primeira camada Win32 usava um "bump allocator": `host_free` era no-op e toda
alocação era arredondada para 4 KiB alinhados a página. Um jogo do porte de GoW
faz milhões de alocações pequenas no boot/loading; o processo inchava até o LMK
do Android matar o app — o "consome toda a RAM e fecha" dos logs do aparelho.
Agora cada bloco é registrado (ptr -> tamanho/alinhamento) numa tabela global;
`free`, `HeapFree`, `HeapReAlloc` (que copiava além do bloco antigo — overread)
e `VirtualFree` devolvem memória de verdade; `realloc` copia min(antigo, novo).
Um teto (default 3072 MiB, override `GOW_HEAP_MB`) faz a alocação falhar
explicitamente (o jogo vê OOM) em vez de o kernel nos matar. O heap reporta
crescimento em bandas de 128 MiB no log, e o sampler de 2 s exibe o RSS.

## D014 — LoadLibrary/GetProcAddress servem pseudo-módulos e thunks hostcall
`GetProcAddress` retornava sempre 0. Um jogo que resolve `D3D11CreateDevice`
dinamicamente guardava NULL e chamava — exatamente a tempestade
`[gow.MISSCALL] target_va=0x0` (e os `badload addr=0x40` de ponteiros quase-nulos
no caminho de erro) dos logs. Agora `LoadLibrary` devolve um pseudo-handle para
qualquer DLL e `GetProcAddress` devolve um thunk hostcall que empacota os
argumentos MS x64 (RCX/RDX/R8/R9 + [rsp+0x28]) e chama o símbolo real — o
backend DXVK Native para d3d11/dxgi/d3dcompiler, o import resolver para o resto.
Na mesma frente foram corrigidos: strings `*W` lidas como ANSI (o teste
`dll_l.ends_with('w')` nunca era verdadeiro — agora é `func.ends_with('W')`),
`CreateThread` com start/param trocados (start é R8, não R9), e os args de pilha
lidos em `arg(4)` em vez de `arg(0)` (disposition do CreateFile, size_lo do
CreateFileMapping, count do MapViewOfFile, whence do SetFilePointer).

## D015 — Imagem guest compartilhada por base entre todas as GowCpu
Cada `GowCpu::new` fazia `mmap MAP_FIXED` no MESMO endereço (0x140000000). Ao
spawnar a primeira thread (`CreateThread` cria uma GowCpu nova), o kernel
SUBSTITUIA o mapeamento que a thread main estava executando: código zerado,
corrupção instantânea e silenciosa. O primeiro mapeamento por base agora vive
num registro global e é compartilhado por todas as CPUs (threads, re-runs da
activity). A GowCpu que o sampler de diagnóstico lê é vazada (uma por processo)
para a thread de monitoramento não ler memória liberada. O log de MISSCALL
ganha deduplicação por alvo distinto (um loop de erro quente não gera mais
megabytes de log por minuto).

## D016 — MapViewOfFile via mmap, não cópia integral
`CreateFileMapping` lia o arquivo INTEIRO para um Vec na RAM (packs do GoW têm
GBs) e `UnmapViewOfFile` não soltava nada. Agora o mapeamento é `mmap MAP_PRIVATE`
do arquivo (páginas sobem do page cache sob demanda), as views são rastreadas e
desmapeadas de verdade; fallback para o Vec se mmap falhar. `CloseHandle` num
mapping fecha o fd e mantém as páginas (MAP_PRIVATE sobrevive sem o fd, igual ao
Windows que permite CloseHandle com views abertas).
