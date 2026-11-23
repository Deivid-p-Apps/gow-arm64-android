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
