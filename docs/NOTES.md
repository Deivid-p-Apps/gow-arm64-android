# NOTES — engenharia reversa

## Cabeçalhos do GoW.exe (medidos)
```
machine=0x8664 (x64)  sections=7  timestamp=1653506097 (2022-05-25)
entry=0x3ec680  image_base=0x140000000  section_alignment=0x1000  file_alignment=0x200
size_of_image=0x519f000

.text    va=0x00001000 vsize=0x00d4649e raw=0x00000400
.rdata   va=0x00d48000 vsize=0x0031139e
.data    va=0x0105a000 vsize=0x04064bd8 (bss-like; não inicializada em disco)
.pdata   va=0x050bf000 vsize=0x00097f5c  -> 0x97f5c/12 = 51.869 RUNTIME_FUNCTION
_RDATA   va=0x05157000
.rsrc    va=0x0515b000
.reloc   va=0x0516f000 vsize=0x0002fbbc (DIR64)
```
- Diretórios: EXPORT rva=0x1054370 (61), IMPORT rva=0x1054f40 (579 imports),
  EXCEPTION rva=0x50bf000, TLS rva=0xf101c0 (callbacks != 0),
  LOAD_CONFIG rva=0xf100c0, IAT rva=0xd48000.

## Convenções
- Chamada: Microsoft x64 (RCX,RDX,R8,R9, shadow space 0x20, alinhamento de 32 bytes).
- GS = TEB. Não há GS em arm64; o lifter trata `gs:`/`fs:` (a completar).
- Canário em `%gs:0x28`; valor em LOAD_CONFIG.

## Camada Sony
A build de PC depende de `libSceGnm.dll`, `libScePad.dll`, `libSceJobManager.dll`,
`libSceGpuAddress.dll` (visíveis no IAT). São de uma camada de conversão PS4/PS5.
O runtime precisa de stubs com as assinaturas de exports usadas pelo IAT.

## Escala e cobertura do lift
- 51.869 funções; 2.754.847 instruções decodificadas.
- **Cobertura: 99,987%** (353 traps de 2.754.847 instruções).
- Restantes não liftados: quase todos I/O de porta privilegiada (`in`/`out`/`insb`/
  `outsb`/`outsd`), instruções privilegiadas (`hlt`/`iret`/`invd`/`wbinvd`/`sgdt`/
  `syscall`), x87 raros (`fild`/`fist`/`fbld`/`fbstp`/`frstor`), `rcl`/`rcr`,
  `lar`/`lsl`, `xgetbv`, `palignr`, `vpackssdw`. Nenhum é crítico para o caminho
  normal do jogo (o código portado as acionaria só em caminhos raros).

## Formato do driver Turnip (adrenotools)
Pacote = zip com `meta.json` + `.so` (tipicamente `libvulkan_freedreno.so`).
Exemplo de meta.json:
```json
{ "schemaVersion":1, "name":"WN-Turnip-1.19-b Axxx", "vendor":"mesa",
  "driverVersion":"WN-1.19-b", "minApi":29,
  "libraryName":"libvulkan_freedreno.so" }
```
Android não permite `dlopen` de armazenamento externo → copiar para o diretório
privado do app e carregar de lá (implementado em `android/.../driver_loader.c`).

## Comandos úteis
```
python3 tools/pe_probe.py <GoW.exe>          # análise de PE
./target/release/gowrecomp <GoW.exe> generated   # lift -> 35 shards + gow_table.c
tools/build_game_so.sh <GoW.exe> out         # lift+compila+linka libgow_game.so
python3 tools/make_flow_pe.py | ./target/release/gowrecomp - flowout  # teste de fluxo
```

## Modelo de fluxo de controle (decisão)
Recompiladores x86 anteriores (trampolim por endereço) quebram porque endereços de
retorno caem no meio de funções. Solução adotada: **chamadas C diretas**
(`f_<rva>(cpu)`) para alvos que são entradas conhecidas; `goto L_<rva>` para ramos
dentro da mesma função; `gow_trampoline`/`gow_call_ptr` para alvos computados
(contados quando desconhecidos). Provado ponta-a-ponta.

## Imports: modelo de chamada inline (importante)
O recompilador traduz `call qword [IAT]` para uma chamada **inline**
`gow_import_call(cpu, iat_rva)` — sem `push` de endereço de retorno. Portanto:
- `gow_import_call` NÃO deve fazer pop nem alterar `next_rva` (corrigido).
- A leitura de argumentos na pilha (`arg(n)` em win32.rs) precisa considerar que
  o endereço de retorno não está empilhado: o 5º argumento fica em `[rsp+0x20]`
  (não `[rsp+0x28]`). Verificar/ajustar os usos de `arg()` na próxima rodada.
