#ifndef GOW_CPU_H
#define GOW_CPU_H
/* Guest CPU state — C view of the Rust `GowCpu` (runtime/src/dispatch.rs).
 * The layout must stay in sync with the #[repr(C)] definition there. The C
 * side only reads/writes the prefix it needs (GPRs, XMM, flags); the object
 * itself is always allocated by Rust and passed in by pointer. */
#include <stdint.h>

typedef struct { uint64_t rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi,
                 r8, r9, r10, r11, r12, r13, r14, r15; } GowGprs;

typedef struct __attribute__((aligned(16))) { uint64_t lo, hi; } GowXmm;

typedef struct GowCpu {
    GowGprs  gpr;
    GowXmm   xmm[17];      /* XMM0..15 + scratch slot 16 */
    uint64_t rip;
    uint64_t flags;
    uint32_t mxcsr;
    uint32_t next_rva;
    /* --- fields below are Rust-side only; opaque to generated code --- */
    uint64_t heap_base;
    uint64_t heap_size;
    uint64_t image_base;
    uint64_t image_size;
    void*    image;
    /* HashMap func_map and the rest follow; C never touches them. */
} GowCpu;

#endif
