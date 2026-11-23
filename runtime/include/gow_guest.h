#ifndef GOW_GUEST_H
#define GOW_GUEST_H
/* Runtime interface seen by recompiled game code (gow_game.c). */
#include <stdint.h>

typedef struct GowCpu GowCpu;

typedef void (*GowFunc)(GowCpu*);
typedef struct { uint32_t rva; GowFunc fn; } GowFuncEntry;

/* Register access: flat 64-bit file, name it GPR */
#define GOW_REG(cpu, name) ((cpu)->gpr.name)

void     gow_unimpl(GowCpu* cpu, uint32_t rva, uint32_t mnemonic);
uint64_t gow_load64(GowCpu* cpu, uint64_t addr);
void     gow_store64(GowCpu* cpu, uint64_t addr, uint64_t v);
uint64_t gow_pop64(GowCpu* cpu);
void     gow_push64(GowCpu* cpu, uint64_t v);
void     gow_setflags(GowCpu* cpu, uint64_t a, uint64_t b, uint64_t r, uint32_t size_bits);

/* Control flow through the address map */
void gow_call(GowCpu* cpu, uint32_t target_rva, uint32_t return_rva);
void gow_call_indirect(GowCpu* cpu, uint32_t target_rva);
void gow_jump(GowCpu* cpu, uint32_t target_rva);
void gow_ret(GowCpu* cpu);

extern const GowFuncEntry gow_func_table[];
extern const unsigned gow_func_count;

#endif
