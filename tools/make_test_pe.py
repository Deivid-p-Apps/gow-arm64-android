#!/usr/bin/env python3
"""Generate a tiny synthetic x86-64 PE32+ for CI self-test of the recompiler.
Not a game: a minimal image with a code section and a fake .pdata entry."""
import struct, sys

def align(x, a): return (x + a - 1) & ~(a - 1)

SECT_ALIGN = 0x1000
FILE_ALIGN = 0x200

# 0x1000: code
code  = bytes([0x48,0x89,0xC8])            # mov rax, rcx
code += bytes([0x48,0x83,0xC0,0x01])       # add rax, 1
code += bytes([0xC3])                      # ret
code += bytes([0x90]*5)
# code ends 0x1000+0x0d
code_len = 0x20
# 0x2000: pdata: one RUNTIME_FUNCTION {start,end,unwind} rva-based
pdata = struct.pack('<III', 0x1000, 0x100d, 0)
pdata_len = 16

def build():
    hdr_size = 0x200
    text_off = hdr_size
    pdata_off = align(text_off + code_len, FILE_ALIGN)
    total = align(pdata_off + pdata_len, FILE_ALIGN)
    b = bytearray(total)
    # DOS header
    b[0:2] = b'MZ'
    struct.pack_into('<I', b, 0x3c, 0x80)
    pe = 0x80
    b[pe:pe+4] = b'PE\0\0'
    # COFF
    struct.pack_into('<H', b, pe+4, 0x8664)   # machine
    struct.pack_into('<H', b, pe+6, 2)        # sections
    struct.pack_into('<I', b, pe+8, 0)        # timestamp
    struct.pack_into('<H', b, pe+20, 0xF0)    # opt size
    struct.pack_into('<H', b, pe+22, 0x22)    # characteristics (EXEC|LARGE)
    opt = pe + 24
    struct.pack_into('<H', b, opt+0, 0x20b)   # PE32+
    struct.pack_into('<I', b, opt+16, 0x1000) # entry rva
    struct.pack_into('<Q', b, opt+24, 0x140000000)  # image base
    struct.pack_into('<I', b, opt+32, SECT_ALIGN)
    struct.pack_into('<I', b, opt+36, FILE_ALIGN)
    struct.pack_into('<I', b, opt+56, 0x3000) # size of image
    struct.pack_into('<I', b, opt+60, FILE_ALIGN)  # size of headers
    struct.pack_into('<H', b, opt+68, 3)      # subsystem CUI
    # data dirs at opt+112
    struct.pack_into('<I', b, opt+112+3*8, 0x2000)   # exception rva
    struct.pack_into('<I', b, opt+112+3*8+4, pdata_len)
    # section headers at opt + F0
    so = opt + 0xF0
    def sec(i, name, va, vsz, raw, rawsz, ch):
        o = so + i*40
        b[o:o+8] = name.encode().ljust(8, b'\0')
        struct.pack_into('<I', b, o+8, vsz)
        struct.pack_into('<I', b, o+12, va)
        struct.pack_into('<I', b, o+16, rawsz)
        struct.pack_into('<I', b, o+20, raw)
        struct.pack_into('<I', b, o+36, ch)
    sec(0, '.text', 0x1000, code_len, text_off, align(code_len, FILE_ALIGN), 0x60000020)
    sec(1, '.pdata', 0x2000, pdata_len, pdata_off, align(pdata_len, FILE_ALIGN), 0x40000040)
    b[text_off:text_off+len(code)] = code
    b[pdata_off:pdata_off+len(pdata)] = pdata
    return bytes(b)

sys.stdout.buffer.write(build())
