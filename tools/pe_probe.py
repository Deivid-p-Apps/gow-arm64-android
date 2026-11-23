#!/usr/bin/env python3
"""Minimal PE32+ probe: headers, sections, imports, exports, TLS, .pdata func count.
No third-party deps. Used for M0 reconnaissance of the target executable."""
import struct, sys, collections

def u16(b,o): return struct.unpack_from('<H',b,o)[0]
def u32(b,o): return struct.unpack_from('<I',b,o)[0]
def u64(b,o): return struct.unpack_from('<Q',b,o)[0]

def rva_to_off(secs, rva):
    for name,va,vs,raw,rawsz in secs:
        if va <= rva < va+max(vs,rawsz):
            return raw + (rva-va)
    return None

def cstr(b,o,lim=512):
    e=b.find(b'\0',o,o+lim)
    if e<0: e=o+lim
    return b[o:e].decode('latin1','replace')

def main(path):
    b=open(path,'rb').read()
    assert b[:2]==b'MZ'
    peoff=u32(b,0x3c)
    assert b[peoff:peoff+4]==b'PE\0\0'
    coff=peoff+4
    machine=u16(b,coff); nsec=u16(b,coff+2)
    tds=u32(b,coff+4); ntsym=u32(b,coff+12); optsz=u16(b,coff+16); chars=u16(b,coff+18)
    opt=coff+20
    magic=u16(b,opt)
    print(f"[HEADER] machine=0x{machine:04x} sections={nsec} timestamp={tds} chars=0x{chars:04x} optmagic=0x{magic:04x}")
    if magic==0x20b:
        entry=u32(b,opt+16); base=u64(b,opt+24); ss=u32(b,opt+32); hs=u32(b,opt+36)
        isz=u32(b,opt+56); imgbase=u64(b,opt+24)
        ddir=opt+112
    else:
        raise SystemExit("not PE32+")
    print(f"[OPT] entry=0x{entry:x} imagebase=0x{base:x} sectalign=0x{ss:x} filealign=0x{hs:x} sizeimage=0x{isz:x}")
    secs=[]
    so=opt+optsz
    for i in range(nsec):
        o=so+i*40
        name=b[o:o+8].rstrip(b'\0').decode('latin1')
        vs=u32(b,o+8); va=u32(b,o+12); rawsz=u32(b,o+16); raw=u32(b,o+20); ch=u32(b,o+36)
        secs.append((name,va,vs,raw,rawsz))
        print(f"[SEC] {name:8s} va=0x{va:08x} vsize=0x{vs:08x} raw=0x{raw:08x} rawsz=0x{rawsz:08x} ch=0x{ch:08x}")
    # data directories
    names={0:'EXPORT',1:'IMPORT',3:'EXCEPTION',9:'TLS',10:'LOAD_CONFIG',12:'IAT',14:'CLR'}
    dirs={}
    for i in range(16):
        rva=u32(b,ddir+i*8); sz=u32(b,ddir+i*8+4)
        dirs[i]=(rva,sz)
        if rva and i in names:
            print(f"[DIR] {names[i]:12s} rva=0x{rva:08x} size=0x{sz:x}")
    # imports
    irva=dirs[1][0]
    if irva:
        off=rva_to_off(secs,irva)
        print("[IMPORTS]")
        while True:
            desc=b[off:off+20]
            oft,ts_,fc,name_rva,first=struct.unpack_from('<IIIII',b,off)
            if name_rva==0 and oft==0: break
            dll=cstr(b,rva_to_off(secs,name_rva))
            n=0
            t=rva_to_off(secs, oft or first)
            while True:
                ent=u64(b,t)
                if ent==0: break
                n+=1; t+=8
            fns=[]
            t=rva_to_off(secs,first)
            cnt=0
            while cnt<6:
                ent=u64(b,t)
                if ent==0: break
                if ent & (1<<63):
                    fns.append(f"ord#{ent&0xffff}")
                else:
                    fns.append(cstr(b,rva_to_off(secs,ent&0x7fffffff),64))
                cnt+=1; t+=8
            print(f"  {dll}: {n} imports  e.g. {', '.join(fns)}")
            off+=20
    # .pdata function table (x64 exception dir)
    er,es=dirs[3]
    if er:
        print(f"[PDATA] exception dir functions = {es//12}")
    # TLS
    tr,ts=dirs[9]
    if tr:
        to=rva_to_off(secs,tr)
        callbacks=u64(b,to+24)
        print(f"[TLS] callbacks=0x{callbacks:x}")
    # scan for AVX/SSE opcodes in .text via capstone-free heuristic
    for name,va,vs,raw,rawsz in secs:
        if name=='.text':
            t=b[raw:raw+rawsz]
            avx = sum(t.count(bytes([c])) for c in (0xc5,0xc4))  # VEX prefixes (rough)
            sse = t.count(0x0f)
            print(f"[CODESCAN] .text size=0x{rawsz:x} VEX-prefix-byte~={avx} 0f-count={sse}")

if __name__=='__main__':
    main(sys.argv[1])
