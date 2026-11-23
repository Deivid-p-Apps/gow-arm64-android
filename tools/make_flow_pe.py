#!/usr/bin/env python3
"""Generate a 2-function synthetic x86-64 PE32+ that exercises call/ret, for
the end-to-end control-flow test (f_1000 calls f_1010; rcx += 5; rax = rcx).
Expected result: rax == 15."""
import struct, sys

def align(x, a): return (x + a - 1) & ~(a - 1)
FA, SA = 0x200, 0x1000

code  = bytes([0xE8, 0x0B, 0x00, 0x00, 0x00])   # call rel32 -> 0x1010
code += bytes([0x48, 0x89, 0xC8])               # mov rax, rcx
code += bytes([0xC3])                           # ret
code += bytes([0x90] * 8)                       # pad to 0x1010
code += bytes([0x48, 0x83, 0xC1, 0x05])         # add rcx, 5
code += bytes([0xC3])                           # ret
code += bytes([0x90] * 16)
clen = 0x30
pdata = struct.pack('<III', 0x1000, 0x1008, 0) + struct.pack('<III', 0x1010, 0x1018, 0)
plen = 24

def build():
    total = align(FA + clen, FA) + align(plen, FA)
    b = bytearray(align(total, FA))
    b[0:2] = b'MZ'; struct.pack_into('<I', b, 0x3c, 0x80)
    pe = 0x80; b[pe:pe+4] = b'PE\0\0'
    struct.pack_into('<H', b, pe+4, 0x8664); struct.pack_into('<H', b, pe+6, 2)
    struct.pack_into('<H', b, pe+20, 0xF0); struct.pack_into('<H', b, pe+22, 0x22)
    opt = pe + 24
    struct.pack_into('<H', b, opt, 0x20b); struct.pack_into('<I', b, opt+16, 0x1000)
    struct.pack_into('<Q', b, opt+24, 0x140000000); struct.pack_into('<I', b, opt+32, SA)
    struct.pack_into('<I', b, opt+36, FA); struct.pack_into('<I', b, opt+56, 0x3000)
    struct.pack_into('<I', b, opt+60, FA); struct.pack_into('<H', b, opt+68, 3)
    struct.pack_into('<I', b, opt+112+3*8, 0x2000); struct.pack_into('<I', b, opt+112+3*8+4, plen)
    so = opt + 0xF0
    def sec(i, name, va, vsz, raw, rawsz, ch):
        o = so + i*40; b[o:o+8] = name.encode().ljust(8, b'\0')
        struct.pack_into('<I', b, o+8, vsz); struct.pack_into('<I', b, o+12, va)
        struct.pack_into('<I', b, o+16, rawsz); struct.pack_into('<I', b, o+20, raw)
        struct.pack_into('<I', b, o+36, ch)
    sec(0, '.text', 0x1000, clen, FA, align(clen, FA), 0x60000020)
    sec(1, '.pdata', 0x2000, plen, align(FA+clen, FA), align(plen, FA), 0x40000040)
    b[FA:FA+len(code)] = code
    po = align(FA+clen, FA); b[po:po+len(pdata)] = pdata
    return bytes(b)

sys.stdout.buffer.write(build())
