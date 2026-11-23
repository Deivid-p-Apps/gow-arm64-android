#!/usr/bin/env python3
"""Header probe for God of War (2018) PC data files.

Prints a best-effort header summary for a file the game ships under
``<install>/exec/``.  It is deliberately conservative:

  * every read is bounded (``seek`` + small window); the big packs
    (``root.lodpack`` ~524 MiB, ``root.texpack`` ~2.5 GiB) are never
    loaded whole,
  * a format is only decoded from bytes that were actually inspected,
  * anything not recognised is reported as ``unknown`` (with a hexdump),
    never guessed.

Findings and their confidence (CONFIRMED vs HYPOTHESIS) are in
``docs/ASSETS.md``.  This tool mirrors that document; it does not invent
anything the document does not state.

Usage:
    python3 tools/asset_probe.py <file> [<file> ...]
    python3 tools/asset_probe.py --dump 256 <file>    # bytes of hexdump
"""

import os
import struct
import sys

# ---------------------------------------------------------------------------
# bounded byte access
# ---------------------------------------------------------------------------

# Hard cap for any single read.  Even "header-ish" formats are small; the
# caps below guarantee we never pull a multi-GiB pack into memory.
MAX_READ = 1 << 20  # 1 MiB


def read_at(path, off, n):
    """Read at most *n* bytes at *off*.  Returns b'' past EOF.  Never reads
    more than MAX_READ bytes in one call."""
    n = max(0, min(n, MAX_READ))
    with open(path, "rb") as f:
        f.seek(off)
        return f.read(n)


def u16(b, o):
    return struct.unpack_from("<H", b, o)[0]


def u32(b, o):
    return struct.unpack_from("<I", b, o)[0]


def u64(b, o):
    return struct.unpack_from("<Q", b, o)[0]


def f32(b, o):
    return struct.unpack_from("<f", b, o)[0]


def cstr(b, o, lim=64):
    end = b.find(b"\0", o, o + lim)
    if end < 0:
        end = o + lim
    return b[o:end].decode("latin1", "replace")


def hexdump(b, base=0, width=32):
    out = []
    for i in range(0, len(b), 16):
        row = b[i:i + 16]
        hexs = " ".join(f"{c:02x}" for c in row)
        txt = "".join(chr(c) if 32 <= c < 127 else "." for c in row)
        out.append(f"    {base + i:08x}  {hexs:<{width + 15}} |{txt}|")
    return "\n".join(out)


# ---------------------------------------------------------------------------
# detectors
# ---------------------------------------------------------------------------

def probe_as(path, size):
    """`.as` — 64 KiB-aligned record bundle (CONFIRMED).

    Each occupied 64 KiB slot starts 0x10 bytes into the slot with the
    constant marker ``03 00 00 00 e2 e2 e2 e2``; the next 0x20 bytes carry
    the record name, preceded by a name hash (u64) and a u32 payload size.
    """
    lines = ["format: AS_BUNDLE (God of War `.as` asset record bundle)"]
    hdr = read_at(path, 0, 0x120)
    if len(hdr) < 0x40 or u32(hdr, 0x10) != 3 or u32(hdr, 0x14) != 0xE2E2E2E2:
        lines.append("  first record marker not at 0x10 -> unexpected layout")
        return lines
    rec = 0x10
    lines.append("  record[0] @ 0x%x" % rec)
    lines.append("    type       = %d        (0x%08x, constant 3 in all 194 non-empty .as)"
                 % (u32(hdr, rec), u32(hdr, rec)))
    lines.append("    fill       = 0x%08x  (constant marker half)" % u32(hdr, rec + 4))
    lines.append("    block_base = 0x%x      (== record offset - 0x10)" % u32(hdr, rec + 8))
    lines.append("    size       = %d bytes (payload size)" % u32(hdr, rec + 0x0C))
    lines.append("    name_hash  = 0x%016x" % u64(hdr, rec + 0x10))
    lines.append("    name       = %r" % cstr(hdr, rec + 0x18))
    lines.append("  layout: records live on 0x10000-byte slots; a record at slot")
    lines.append("          base B has marker at B+0x10.  Slots inside a large record")
    lines.append("          carry no marker (payload spans them).")
    lines.append("  NOTE: this bundle has %d bytes = %d slots of 64 KiB"
                 % (size, (size + 0xFFFF) // 0x10000))
    return lines


def probe_wad(path, size):
    """`.wad` — Wwise-referencing asset wad (CONFIRMED magic only)."""
    lines = ["format: WAD (God of War `.wad` asset wad)"]
    hdr = read_at(path, 0, 0x100)
    lines.append("  const u32@0x00 = %d      (0x%08x, 0x15 in all 597 .wad)" % (u32(hdr, 0), u32(hdr, 0)))
    lines.append("  const u32@0x04 = %d      (0x08 in all 597 .wad)" % u32(hdr, 4))
    marker = hdr[8:20]
    lines.append("  section signature @0x08 = %s (aa aa aa ba + 8x aa)" % marker.hex(" "))
    # first record: 12-byte marker, u32 tag, 0x20-byte name, sentinel, fields
    lines.append("  record[0]:")
    lines.append("    tag u32 @0x14 = 0x%08x" % u32(hdr, 0x14))
    lines.append("    name    @0x18 = %r" % cstr(hdr, 0x18, 0x20))
    lines.append("    sentinel @0x50 = %r" % cstr(hdr, 0x50, 16))
    lines.append("    u32 @0x60 = %d, u32 @0x64 = %d" % (u32(hdr, 0x60), u32(hdr, 0x64)))
    lines.append("  layout: repeating records, each starting with the 12-byte")
    lines.append("          signature, a 0x20-byte ASCII name, and the ASCII")
    lines.append("          sentinel 'YouLoveFromMaya' before LE fields.")
    return lines


def probe_pack_toc(path, size, which):
    """`.lodpack.toc` / `.texpack.toc` (and the pack files themselves)."""
    lines = ["format: %s pack" % which.upper()]
    hdr = read_at(path, 0, 0x40)
    if which == "lodpack":
        # CONFIRMED for root.lodpack.toc and every chunk *.lodpack.toc:
        #   0x00 u32 A, 0x04 u32 B, 0x08 u32 0, 0x0C u32 1
        #   then A entries of 0x18 bytes, then B entries of 0x18 bytes.
        A, B = u32(hdr, 0), u32(hdr, 4)
        lines.append("  u32@0x00 A = %d   (table A entries)" % A)
        lines.append("  u32@0x04 B = %d   (table B entries)" % B)
        lines.append("  u32@0x08   = %d" % u32(hdr, 8))
        lines.append("  u32@0x0C   = %d" % u32(hdr, 0x0C))
        expect = 0x10 + A * 0x18 + B * 0x18
        if path.endswith(".toc"):
            lines.append("  layout 0x10 + A*0x18 + B*0x18 = %d  (file is %d) %s"
                         % (expect, size, "OK" if expect == size else "MISMATCH"))
        else:
            lines.append("  TOC header size  0x10 + A*0x18 + B*0x18 = %d bytes" % expect)
            lines.append("  file is the pack *body*; its first %d bytes are the TOC" % expect)
            lines.append("  (a sibling .toc file of %d bytes exists)" % expect)
        lines.append("  table A entry (0x18): u64 offset, u64 name_hash, u32 size, u32 flag")
        lines.append("  table B entry (0x18): u32 a, u32 b, u64 name_hash, u32 size, u32 flag")
        if A:
            b = read_at(path, 0x10, 0x18 * min(A, 4))
            for i in range(len(b) // 0x18):
                o = i * 0x18
                lines.append("    A[%d] off=%-12d hash=0x%016x size=%-10d flag=%d"
                             % (i, u64(b, o), u64(b, o + 8), u32(b, o + 0x10), u32(b, o + 0x14)))
        if B:
            b = read_at(path, 0x10 + A * 0x18, 0x18 * min(B, 2))
            for i in range(len(b) // 0x18):
                o = i * 0x18
                lines.append("    B[%d] a=%-10d b=%-10d hash=0x%016x size=%-10d flag=%d"
                             % (i, u32(b, o), u32(b, o + 4), u64(b, o + 8), u32(b, o + 0x10), u32(b, o + 0x14)))
    else:
        # texpack.toc: only the self-size field is CONFIRMED.  Entry layout is
        # UNRESOLVED (see docs/ASSETS.md) so we deliberately print no entries.
        lines.append("  u32@0x20 = %d   (self size; matches file length %d: %s)"
                     % (u32(hdr, 0x20), size, u32(hdr, 0x20) == size))
        lines.append("  u32@0x24 = %d   (count-like)" % u32(hdr, 0x24))
        lines.append("  u32@0x28 = %d   (first data region offset)" % u32(hdr, 0x28))
        lines.append("  u32@0x2C = %d" % u32(hdr, 0x2C))
        lines.append("  u32@0x30 = %d" % u32(hdr, 0x30))
        lines.append("  ENTRY LAYOUT UNRESOLVED: do not parse without re-deriving it.")
    if path.endswith((".lodpack", ".texpack")):
        lines.append("  NOTE: this is the pack *body*, not a .toc.")
    return lines


def probe_shaderpack(path, size):
    lines = ["format: SHDRPACK (GPU shader pack, CONFIRMED magic/version/count)"]
    hdr = read_at(path, 0, 0x40)
    lines.append("  magic  @0x00 = %r" % hdr[:8].decode("latin1", "replace"))
    lines.append("  u64@0x08 version = %d" % u64(hdr, 8))
    lines.append("  u64@0x10 entries = %d" % u64(hdr, 0x10))
    lines.append("  u64@0x18 = %d" % u64(hdr, 0x18))
    lines.append("  u64@0x20 = %d" % u64(hdr, 0x20))
    lines.append("  u64@0x28 = %d" % u64(hdr, 0x28))
    lines.append("  u64@0x30 = %d" % u64(hdr, 0x30))
    lines.append("  record layout past the header: HYPOTHESIS, not decoded here.")
    return lines


def probe_bink2(path, size):
    lines = ["format: BINK2 video (`.bk2`, CONFIRMED)"]
    hdr = read_at(path, 0, 0x40)
    lines.append("  magic @0x00      = %r" % hdr[:4].decode("latin1", "replace"))
    lines.append("  u32@0x04 size-8   = %d  (file-8 = %d) %s"
                 % (u32(hdr, 4), size - 8, "OK" if u32(hdr, 4) == size - 8 else "MISMATCH"))
    lines.append("  u32@0x08 frames?  = %d" % u32(hdr, 8))
    lines.append("  u32@0x0C maxframe = %d" % u32(hdr, 0x0C))
    lines.append("  u32@0x10 frames?  = %d" % u32(hdr, 0x10))
    lines.append("  u32@0x14 width    = %d" % u32(hdr, 0x14))
    lines.append("  u32@0x18 height   = %d" % u32(hdr, 0x18))
    lines.append("  u32@0x1C fps      = %d  (<<16 fixed-point convention)" % u32(hdr, 0x1C))
    lines.append("  u32@0x20          = %d" % u32(hdr, 0x20))
    lines.append("  u32@0x24          = %d" % u32(hdr, 0x24))
    return lines


def probe_wem(path, size):
    lines = ["format: WEM / Wwise RIFF (CONFIRMED)"]
    hdr = read_at(path, 0, 0x60)
    lines.append("  magic @0x00 = %r  u32@0x04 riffsize = %d (file-8 = %d)"
                 % (hdr[:4].decode("latin1", "replace"), u32(hdr, 4), size - 8))
    lines.append("  form  @0x08 = %r" % hdr[8:12].decode("latin1", "replace"))
    if hdr[12:16] == b"fmt ":
        lines.append("  fmt   @0x0C = %r  chunk_len = %d" % ("fmt ", u32(hdr, 16)))
        lines.append("    wFormatTag  = 0x%04x  (0xFFFF = Wwise Vorbis)" % u16(hdr, 20))
        lines.append("    channels    = %d" % u16(hdr, 22))
        lines.append("    sampleRate  = %d" % u32(hdr, 24))
    d = hdr.find(b"data")
    if d >= 0:
        lines.append("  data  @0x%02x, dataSize = %d" % (d, u32(hdr, d + 4)))
    return lines


def probe_sbp(path, size):
    lines = ["format: SBP (Wwise soundbank, CONFIRMED header fields)"]
    hdr = read_at(path, 0, 0x40)
    lines.append("  u32@0x00 = 0x%08x  (constant 0x00050001 in sampled banks)" % u32(hdr, 0))
    lines.append("  u32@0x04 = %d" % u32(hdr, 4))
    lines.append("  sentinel @0x08 = %s" % hdr[8:24].hex(" "))
    lines.append("  name     @0x18 = %r" % cstr(hdr, 0x18, 0x20))
    lines.append("  record layout: HYPOTHESIS, not decoded here.")
    return lines


def probe_dcb(path, size):
    lines = ["format: DCB (data container, header fields CONFIRMED, layout HYPOTHESIS)"]
    hdr = read_at(path, 0, 0x40)
    lines.append("  u16@0x00 = %d  u16@0x02 = %d  u32@0x04 = %d"
                 % (u16(hdr, 0), u16(hdr, 2), u32(hdr, 4)))
    lines.append("  name @0x18 = %r" % cstr(hdr, 0x18, 0x20))
    return lines


def probe_wypdb(path, size):
    lines = ["format: WYPDB (pipeline-database sidecar, HYPOTHESIS)"]
    hdr = read_at(path, 0, 0x40)
    lines.append("  u16@0x00 = %d  u16@0x02 = %d  u32@0x04 = %d (file-108 = %d)"
                 % (u16(hdr, 0), u16(hdr, 2), u32(hdr, 4), size - 108))
    lines.append("  name @0x18 = %r" % cstr(hdr, 0x18, 0x20))
    lines.append("  contains 'RLTD' chunk tag at fixed offsets: HYPOTHESIS")
    return lines


def probe_json(path, size):
    lines = ["format: JSON (plain text, CONFIRMED)"]
    head = read_at(path, 0, 64)
    lines.append("  starts with %r" % head[:16].decode("latin1", "replace"))
    lines.append("  line endings: %s" % ("CRLF" if b"\r\n" in read_at(path, 0, 4096) else "LF"))
    return lines


def probe_csv(path, size):
    lines = ["format: CSV (CONFIRMED: '<hash>,<small int>' per line)"]
    head = read_at(path, 0, 128)
    lines.append("  first record: %r" % head.split(b"\n", 1)[0].decode("latin1", "replace"))
    return lines


def probe_ini(path, size):
    lines = ["format: ASCII text / INI (CONFIRMED text)"]
    head = read_at(path, 0, 256)
    eol = "CRLF" if b"\r\n" in read_at(path, 0, 4096) else "LF"
    lines.append("  encoding: ASCII, line endings: %s" % eol)
    for ln in head.splitlines()[:8]:
        lines.append("    %s" % ln.decode("latin1", "replace"))
    return lines


def probe_pe(path, size):
    lines = ["format: PE executable (use tools/pe_probe.py for the full dump)"]
    hdr = read_at(path, 0, 0x40)
    lines.append("  MZ @0x00 = %r" % hdr[:2].decode("latin1", "replace"))
    return lines


def detect(path):
    """Return (kind, probe_fn) using extension first, then magic bytes."""
    ext = os.path.splitext(path)[1].lower()
    name = os.path.basename(path).lower()
    head = read_at(path, 0, 0x20)
    if len(head) >= 2 and head[:2] == b"MZ":
        return "pe", probe_pe
    if head[:8] == b"SHDRPACK":
        return "shaderpack", probe_shaderpack
    if head[:4] == b"KB2j":
        return "bink2", probe_bink2
    if head[:4] == b"RIFF" and head[8:12] == b"WAVE":
        return "wem", probe_wem
    # `.as` marker: some records start at 0x10 (not 0x00).
    if len(head) >= 0x18 and head[0x10:0x14] == b"\x03\x00\x00\x00" and head[0x14:0x18] == b"\xe2\xe2\xe2\xe2":
        return "as", probe_as
    if head[8:16] == b"\xaa\xaa\xaa\xba\xaa\xaa\xaa\xaa" and ext == ".wad":
        return "wad", probe_wad
    if name.endswith(".lodpack.toc"):
        return "lodpack.toc", lambda p, s: probe_pack_toc(p, s, "lodpack")
    if name.endswith(".lodpack"):
        return "lodpack", lambda p, s: probe_pack_toc(p, s, "lodpack")
    if name.endswith(".texpack.toc") or name.endswith(".texpack"):
        return "texpack", lambda p, s: probe_pack_toc(p, s, "texpack")
    if ext == ".wad":
        return "wad", probe_wad
    if ext == ".as":
        return "as", probe_as
    if ext == ".sbp":
        return "sbp", probe_sbp
    if ext == ".dcb":
        return "dcb", probe_dcb
    if ext == ".wypdb":
        return "wypdb", probe_wypdb
    if ext == ".json":
        return "json", probe_json
    if ext == ".csv":
        return "csv", probe_csv
    if ext in (".ini", ".txt", ".conf", ".log"):
        return "text", probe_ini
    if ext == ".bk2":
        return "bink2", probe_bink2
    if ext == ".wem":
        return "wem", probe_wem
    return "unknown", None


def probe(path, dump_bytes=32):
    if not os.path.isfile(path):
        print("== %s ==\n  not a regular file" % path)
        return
    size = os.path.getsize(path)
    kind, fn = detect(path)
    print("== %s ==" % path)
    print("  size: %d bytes (%.2f MiB)" % (size, size / (1 << 20)))
    if fn is None:
        print("  format: unknown (no recognised signature)")
        print("  first %d bytes:" % dump_bytes)
        print(hexdump(read_at(path, 0, dump_bytes)))
        return
    for line in fn(path, size):
        print(line)
    print("  header dump:")
    print(hexdump(read_at(path, 0, dump_bytes)))


def main(argv):
    dump = 32
    args = []
    i = 0
    while i < len(argv):
        if argv[i] == "--dump":
            i += 1
            dump = int(argv[i], 0)
        else:
            args.append(argv[i])
        i += 1
    if not args:
        print(__doc__)
        return 1
    for p in args:
        probe(p, dump)
        print()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
