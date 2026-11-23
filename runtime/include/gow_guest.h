#ifndef GOW_GUEST_H
#define GOW_GUEST_H
/* Runtime interface seen by recompiled game code (gow_game.c).
 *
 * Control flow model: generated functions do not call each other directly.
 * They set the next function through the runtime and return (C return) to the
 * central dispatch loop in gow_run(). This makes call/ret/tail-jmp exact. */
#include <stdint.h>

#include "gow_cpu.h"

typedef void (*GowFunc)(GowCpu*);
typedef struct { uint32_t rva; GowFunc fn; } GowFuncEntry;

/* Register access into the flat 64-bit GPR file. */
#define GOW_REG(cpu, name) ((cpu)->gpr.name)
#define GOW_FLAGS(cpu) ((cpu)->flags)

/* --- integer ------------------------------------------------------------ */
uint64_t gow_add64(GowCpu* cpu, uint64_t a, uint64_t b, uint32_t size_bits);
uint64_t gow_sub64(GowCpu* cpu, uint64_t a, uint64_t b, uint32_t size_bits);
uint64_t gow_logic64(GowCpu* cpu, uint64_t r, uint32_t size_bits);
uint64_t gow_shift(GowCpu* cpu, uint64_t v, uint64_t n, uint32_t size_bits, uint32_t op);
uint64_t gow_bt(GowCpu* cpu, uint64_t a, uint64_t i, uint32_t size_bits, uint32_t op);
uint64_t gow_bsf(GowCpu* cpu, uint64_t v);
uint64_t gow_bsr(GowCpu* cpu, uint64_t v);
void     gow_stos(GowCpu* cpu, uint32_t size);
void     gow_lahf(GowCpu* cpu);
void     gow_sahf(GowCpu* cpu);
void     gow_cpuid(GowCpu* cpu);
uint64_t gow_test_cc(GowCpu* cpu, uint32_t cc); /* 0=O,1=NO,2=B,3=AE,4=E,5=NE,6=BE,7=A,8=S,9=NS,10=P,11=NP,12=L,13=GE,14=LE,15=G */

/* --- memory ------------------------------------------------------------- */
uint64_t gow_load(GowCpu* cpu, uint64_t addr, uint32_t size);   /* size 1/2/4/8 */
void     gow_store(GowCpu* cpu, uint64_t addr, uint64_t v, uint32_t size);
uint64_t gow_pop64(GowCpu* cpu);
void     gow_push64(GowCpu* cpu, uint64_t v);
void     gow_load128(GowCpu* cpu, uint64_t addr, uint8_t out[16]);
void     gow_store128(GowCpu* cpu, uint64_t addr, const uint8_t v[16]);
void     gow_load_xmm(GowCpu* cpu, uint32_t reg, uint64_t addr);
void     gow_store_xmm(GowCpu* cpu, uint32_t reg, uint64_t addr);

/* --- control flow (set next + return to loop) --------------------------- */
void gow_call(GowCpu* cpu, uint32_t target_rva, uint32_t return_rva);
void gow_jump(GowCpu* cpu, uint32_t target_rva);   /* also used for jcc taken */
void gow_call_indirect(GowCpu* cpu, uint64_t target_va);
void gow_ret(GowCpu* cpu);
void gow_import_call(GowCpu* cpu, uint32_t iat_rva); /* Win32/NT API from IAT */
void gow_trampoline(GowCpu* cpu, uint32_t target_rva);
void gow_call_ptr(GowCpu* cpu, uint64_t target_va);

/* --- xmm helpers (scalar + packed, minimal but exact) ------------------- */
void     gow_xmm_movaps(GowCpu* cpu, int dst, int src);
void     gow_xmm_pxor(GowCpu* cpu, int dst, int src);
void     gow_xmm_padd(GowCpu* cpu, int dst, int src, uint32_t size);
void     gow_xmm_psub(GowCpu* cpu, int dst, int src, uint32_t size);
void     gow_xmm_pand(GowCpu* cpu, int dst, int src);
void     gow_xmm_por(GowCpu* cpu, int dst, int src);
void     gow_xmm_pcmpeq(GowCpu* cpu, int dst, int src, uint32_t size);
void     gow_xmm_arith_ps(GowCpu* cpu, int dst, int src, uint32_t op); /* 0=add 1=sub 2=mul 3=div */
void     gow_xmm_arith_ss(GowCpu* cpu, int dst, int src, uint32_t op);
uint64_t gow_xmm_read_u32(GowCpu* cpu, int reg, uint32_t lane);
void     gow_xmm_write_u32(GowCpu* cpu, int reg, uint32_t lane, uint64_t v);
void     gow_xmm_unpck(GowCpu* cpu, int dst, int src, uint32_t elem, uint32_t hi);
void     gow_xmm_punpck(GowCpu* cpu, int dst, int src, uint32_t elem, uint32_t hi);
void     gow_xmm_shift(GowCpu* cpu, int dst, uint32_t bytes, uint32_t op, uint32_t size);
void     gow_xmm_pshufd(GowCpu* cpu, int dst, int src, uint32_t imm);
void     gow_xmm_pmovmskb(GowCpu* cpu, int dst, int src);
uint64_t gow_imul64(GowCpu* cpu, uint64_t a, uint64_t b, uint32_t size_bits);
void     gow_mul_rax_rdx(GowCpu* cpu, uint64_t b, uint32_t size_bits);
void     gow_div_rax_rdx(GowCpu* cpu, uint64_t b, uint32_t size_bits, uint32_t signed_);
int64_t  gow_div_sse(GowCpu* cpu, int64_t a, int64_t b);
uint64_t gow_popfq(GowCpu* cpu);
void     gow_pushfq(GowCpu* cpu);

void     gow_xmm_pandn(GowCpu* cpu, int dst, int src);
void     gow_xmm_pcmpgt(GowCpu* cpu, int dst, int src, uint32_t size);
void     gow_xmm_pmax(GowCpu* cpu, int dst, int src, uint32_t size, uint32_t sign);
void     gow_xmm_arith_pd(GowCpu* cpu, int dst, int src, uint32_t op);
void     gow_xmm_arith_sd(GowCpu* cpu, int dst, int src, uint32_t op);
void     gow_xmm_sqrt(GowCpu* cpu, int dst, uint32_t is_double, uint32_t scalar);
void     gow_xmm_rsqrt(GowCpu* cpu, int dst);
void     gow_xmm_rcp(GowCpu* cpu, int dst);
void     gow_xmm_minmax(GowCpu* cpu, int dst, int src, uint32_t max, uint32_t mode);
void     gow_xmm_movhl(GowCpu* cpu, int dst, int src);
void     gow_xmm_movlh(GowCpu* cpu, int dst, int src);
void     gow_xmm_shufps(GowCpu* cpu, int dst, int src, uint32_t imm);
void     gow_xmm_shufpd(GowCpu* cpu, int dst, int src, uint32_t imm);
void     gow_xmm_movmsk(GowCpu* cpu, int dst, int src);
void     gow_xmm_comis(GowCpu* cpu, int dst, int src, uint32_t is_double);
void     gow_xmm_cmpps(GowCpu* cpu, int dst, int src, uint32_t pred, uint32_t kind);
void     gow_xmm_andn(GowCpu* cpu, int dst, int src);
void     gow_xmm_from_gpr(GowCpu* cpu, int xreg, uint64_t val, uint32_t movq);
void     gow_xmm_to_gpr(GowCpu* cpu, int xreg, uint32_t movq, uint32_t dst_idx);
void     gow_xmm_cvt_xx(GowCpu* cpu, int dst, int src, uint32_t op);
void     gow_xmm_cvt_g2x(GowCpu* cpu, int dst, uint32_t op, uint64_t val);
uint64_t gow_xmm_cvt_x2g(GowCpu* cpu, int src, uint32_t op);
void     gow_xmm_to_gpr_val(GowCpu* cpu, int dst, uint64_t v);
uint64_t gow_xmm_lane0(GowCpu* cpu, int reg);
void     gow_xmm_movd_reg(GowCpu* cpu, int dst, int src, uint32_t movq);
uint64_t gow_xmm_movmsk_val(GowCpu* cpu, int src);
void     gow_movs(GowCpu* cpu, uint32_t size);
void     gow_lods(GowCpu* cpu, uint32_t size);
void     gow_scas(GowCpu* cpu, uint32_t size);
void     gow_cmps(GowCpu* cpu, uint32_t size);
void     gow_enter(GowCpu* cpu, uint64_t frame, uint64_t level);
void     gow_ldmxcsr(GowCpu* cpu, uint64_t addr);
void     gow_stmxcsr(GowCpu* cpu, uint64_t addr);
void     gow_unimpl(GowCpu* cpu, uint32_t rva, uint32_t mnemonic);

/* Drive recompiled code starting at a function entry RVA. */
void     gow_run(GowCpu* cpu, uint32_t entry_rva);

#endif
