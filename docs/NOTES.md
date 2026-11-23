# NOTES — engenharia reversa

## Cabeçalhos do GoW.exe (medidos)
```
machine=0x8664 (x64)  sections=7  timestamp=1653506097 (2022-05-25)
entry=0x3ec680  image_base=0x140000000  section_alignment=0x1000  file_alignment=0x200
size_of_image=0x519f000

.text    va=0x00001000 vsize=0x00d4649e raw=0x00000400
.rdata   va=0x00d48000 vsize=0x0031139e
.data    va=0x0105a000 vsize=0x04064bd8 (não inicializada em disco: bss-like)
.pdata   va=0x050bf000 vsize=0x00097f5c   -> 0x97f5c/12 = 51.869 RUNTIME_FUNCTION
_RDATA   va=0x05157000
.rsrc    va=0x0515b000
.reloc   va=0x0516f000 vsize=0x0002fbbc  (DIR64)
```
- Diretórios: EXPORT rva=0x1054370 (61), IMPORT rva=0x1054f40, EXCEPTION rva=0x50bf000,
  TLS rva=0xf101c0 (callbacks != 0), LOAD_CONFIG rva=0xf100c0, IAT rva=0xd48000.

## Convenções
- Chamada: Microsoft x64 (RCX,RDX,R8,R9, shadow space 0x20, 32-byte align na entrada de call).
- Registrador GS = TEB (Windows). A camada de runtime precisa prover TEB em GS/`gs:[ ]`.
  No arm64 não há GS; o lifter reescreve `gs:` para lookup no TEB host (a implementar).
- Canário de stack em `%gs:0x28`; load config tem o valor (ver LOAD_CONFIG dir).

## Camada Sony
A build de PC carrega/depende de `libSceGnm.dll`, `libScePad.dll`, `libSceJobManager.dll`,
`libSceGpuAddress.dll` (imports visíveis no IAT). São de uma camada de conversão de PS4/PS5.
O runtime precisa de stubs dessas DLLs com as assinaturas de exports usadas pelo IAT.

## Escala
- 51.869 funções; 2.756.013 instruções decodificadas no lift atual.
- Cobertura de mnemonics: 292 distintos ainda não liftados (ver lift.log), dominados por
  SSE/SSE2 (movups/addps/mulps/pand/pcmp*/padd*/...), cmov*, adc/sbb, bsf/bsr/bt*, string ops.

## Comandos úteis
```
python3 tools/pe_probe.py <GoW.exe>          # análise de PE
./target/release/gowrecomp <GoW.exe> generated   # lifta tudo -> generated/gow_game.c + manifest.json
```
