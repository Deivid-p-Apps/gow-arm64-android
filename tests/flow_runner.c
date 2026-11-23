//! End-to-end control-flow test: compile generated shards + this harness and
//! run a real program through the dispatch loop on the host. Proves that
//! call/ret/jmp and flags work on actually-lifted code (not just unit helpers).
//!
//! Usage (from repo root, after `gowrecomp <flow.bin> ctest/out`):
//!   clang -O2 -I runtime/include ctest/flow_runner.c ctest/out/*.c \
//!     -L target/release -lgow_runtime -lpthread -o ctest/flow_runner
//!   ./ctest/flow_runner

#include "gow_guest.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Provided by the generated gow_table.c */
extern const GowFuncEntry gow_func_table[];
extern const unsigned gow_func_count;

/* Rust side: allocate a GowCpu and register the table. */
extern void* gow_cpu_new(unsigned long image_base, unsigned long image_size);
extern void gow_register_fn(void* cpu, unsigned rva, void* fn);
extern void gow_run_c(uintptr_t cpu, unsigned entry_rva);
extern unsigned long gow_get_gpr(void* cpu, int idx); /* 0=rax,1=rcx,... */
extern void gow_set_gpr(void* cpu, int idx, unsigned long v);

int main(void) {
    void* cpu = gow_cpu_new(0x140000000UL, 0x10000);
    for (unsigned i = 0; gow_func_table[i].fn; i++) {
        gow_register_fn(cpu, gow_func_table[i].rva, (void*)gow_func_table[i].fn);
    }
    gow_set_gpr(cpu, 1, 10);            /* rcx = 10 */
    gow_run_c((uintptr_t)cpu, 0x1000);  /* run f_1000 */
    /* f_1000: call f_1010 (rcx += 5 -> 15), then mov rax, rcx */
    unsigned long rax = gow_get_gpr(cpu, 0);
    printf("rax=%lu\n", rax);
    return rax == 15 ? 0 : 1;
}
