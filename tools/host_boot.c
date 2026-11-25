// Host boot harness: runs the REAL recompiled God of War through the runtime on
// the host (Termux aarch64), no APK/device/DXVK needed. It exercises the lifter
// + Win32/CRT layer end to end so logic bugs surface in seconds like the CI test.
//
// Build (from repo root):
//   cargo build -p gow-runtime --release
//   gowrecomp /storage/emulated/0/god/GodOfWar/GoW.exe generated
//   clang -O1 -I runtime/include -I generated host_boot.c generated/gow_shard_*.c \
//     generated/gow_table.c -L target/release -lgow_runtime -lpthread -ldl \
//     -Wl,-rpath,target/release -o host_boot
//   ./host_boot /storage/emulated/0/god
#include "gow_guest.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern const GowFuncEntry gow_func_table[];
extern const unsigned gow_func_count;

// Rust runtime entry points.
extern void* gow_cpu_new(unsigned long image_base, unsigned long image_size);
extern void gow_register_fn(void* cpu, unsigned rva, void* fn);
extern void gow_run_c(uintptr_t cpu, unsigned entry_rva);
extern unsigned long gow_get_gpr(void* cpu, int idx);

// PE loader + resolver (Rust). Takes the generated function table.
extern int host_boot_run(const char* gamedir, const GowFuncEntry* table);

int main(int argc, char** argv) {
    const char* gdir = argc > 1 ? argv[1] : "/storage/emulated/0/god";
    fprintf(stderr, "[host_boot] gamedir=%s funcs=%u\n", gdir, gow_func_count);
    int rc = host_boot_run(gdir, gow_func_table);
    fprintf(stderr, "[host_boot] returned %d\n", rc);
    return rc;
}
