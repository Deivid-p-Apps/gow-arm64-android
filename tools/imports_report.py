#!/usr/bin/env python3
"""imports_report.py - enumerate EVERY import the target PE32+ uses.

Reads the PE import directory (the same data the loader uses to build the IAT)
and prints one entry per (dll, function) pair. Ordinal imports are reported as
``ord#N`` (the imported-by-ordinal form), exactly what the runtime has to match.

Outputs:
  * default      : human table, one row per DLL with its functions
  * --json       : machine-readable {"dlls": {...}, "imports": [...]} document
  * --coverage   : additionally cross-check the imports against the set of
                   (dll, func) pairs handled by the runtime and report
                   per-DLL implemented / missing counts. The runtime set is
                   parsed from the source, not hard-coded, so the report stays
                   honest if the runtime changes.

No third-party dependencies. Usage:
    python3 tools/imports_report.py <GoW.exe> [--json] [--coverage [--root DIR]]
"""
import argparse
import collections
import json
import os
import re
import struct
import sys

# ---------------------------------------------------------------------------
# PE parsing
# ---------------------------------------------------------------------------

def u16(b, o):
    return struct.unpack_from('<H', b, o)[0]


def u32(b, o):
    return struct.unpack_from('<I', b, o)[0]


def u64(b, o):
    return struct.unpack_from('<Q', b, o)[0]


def rva_to_off(sections, rva):
    for name, va, vsize, raw, rawsize in sections:
        if va <= rva < va + max(vsize, rawsize):
            return raw + (rva - va)
    return None


def cstr(b, o, limit=1024):
    if o is None or o < 0 or o >= len(b):
        return ""
    end = b.find(b'\0', o, o + limit)
    if end < 0:
        end = min(o + limit, len(b))
    return b[o:end].decode('latin1', 'replace')


class PE(object):
    """Just enough PE32+ to read the import directory and the IAT slots."""

    def __init__(self, path):
        self.path = path
        with open(path, 'rb') as f:
            self.b = b = f.read()
        if b[:2] != b'MZ':
            raise SystemExit("not a PE file (missing MZ)")
        peoff = u32(b, 0x3c)
        if b[peoff:peoff + 4] != b'PE\0\0':
            raise SystemExit("not a PE file (missing PE signature)")
        coff = peoff + 4
        self.machine = u16(b, coff)
        nsec = u16(b, coff + 2)
        optsz = u16(b, coff + 16)
        opt = coff + 20
        magic = u16(b, opt)
        if magic != 0x20b:
            raise SystemExit("only PE32+ (x64) supported, got optmagic 0x%x" % magic)
        self.sections = []
        so = opt + optsz
        for i in range(nsec):
            o = so + i * 40
            name = b[o:o + 8].rstrip(b'\0').decode('latin1')
            vsize = u32(b, o + 8)
            va = u32(b, o + 12)
            rawsize = u32(b, o + 16)
            raw = u32(b, o + 20)
            self.sections.append((name, va, vsize, raw, rawsize))
        self.entry_rva = u32(b, opt + 16)
        self.image_base = u64(b, opt + 24)
        self.size_of_image = u32(b, opt + 56)
        self.dirs = {}
        ddir = opt + 112
        for i in range(16):
            self.dirs[i] = (u32(b, ddir + i * 8), u32(b, ddir + i * 8 + 4))

    # -- imports ---------------------------------------------------------
    def imports(self):
        """Return [{'dll','func','ordinal','iat_rva'}...] in IAT order.

        Mirrors the loader: walk the import descriptors; for each, walk the
        OriginalFirstThunk (names) and the FirstThunk (IAT slot RVA). A 1<<63
        bit means the entry is ``ord#N``.
        """
        out = []
        imp_rva, _ = self.dirs[1]
        if not imp_rva:
            return out
        off = rva_to_off(self.sections, imp_rva)
        if off is None:
            return out
        while True:
            oft = u32(self.b, off)
            name_rva = u32(self.b, off + 12)
            first = u32(self.b, off + 16)
            if oft == 0 and name_rva == 0 and first == 0:
                break
            dll = cstr(self.b, rva_to_off(self.sections, name_rva)) if name_rva else ""
            thunk_rva = oft if oft else first
            t = rva_to_off(self.sections, thunk_rva)
            if t is not None:
                slot = 0
                while True:
                    ent = u64(self.b, t)
                    if ent == 0:
                        break
                    if ent & (1 << 63):
                        func = "ord#%d" % (ent & 0xffff)
                        ordinal = ent & 0xffff
                    else:
                        # IMAGE_IMPORT_BY_NAME = 2-byte hint, then the name.
                        func = cstr(self.b, rva_to_off(self.sections, ent & 0x7fffffff) + 2)
                        ordinal = None
                    out.append({
                        'dll': dll,
                        'func': func,
                        'ordinal': ordinal,
                        'iat_rva': first + slot * 8,
                    })
                    t += 8
                    slot += 1
            off += 20
        return out


# ---------------------------------------------------------------------------
# Runtime coverage
# ---------------------------------------------------------------------------
# The runtime resolves an IAT slot by matching ("dll", "func") (dll
# lower-cased) in ImportResolver::call. We parse that source rather than
# duplicating it, so the audit cannot silently drift.

_ARM_LITERAL = re.compile(r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)')
_ARM_DLLVAR = re.compile(r'\(\s*"([^"]+)"\s*,\s*(f|_)\s*\)')


def parse_runtime(root):
    """Return (implemented, wildcard_dlls, stubs, files) parsed from runtime/src.

    implemented   : set of (dll_lower, func) handled literally (or via a group).
    wildcard_dlls : set of dll_lower whose whole match arm is handled
                    (pattern ``("dll", f)`` or ``("dll", _)``).
    stubs         : dict name -> set of funcs, for helper functions that are
                    NOT reached from ImportResolver::call (steam.rs).
    files         : the source files actually read.
    """
    rsrc = os.path.join(root, 'runtime', 'src')
    implemented = set()
    wildcard_dlls = set()
    stubs = {}
    files = []

    win32 = os.path.join(rsrc, 'win32.rs')
    if os.path.exists(win32):
        files.append(win32)
        src = open(win32, encoding='utf-8', errors='replace').read()
        # Only look at the body of ImportResolver::call (the dispatch match).
        start = src.find('pub unsafe fn call')
        end = src.find('unsafe fn resolve_proc')
        body = src[start:end] if start >= 0 and end > start else src
        for line in body.splitlines():
            if '=>' not in line and '|' not in line:
                continue
            for dll, func in _ARM_LITERAL.findall(line):
                implemented.add((dll.lower(), func))
            for dll, _var in _ARM_DLLVAR.findall(line):
                wildcard_dlls.add(dll.lower())

    gfx = os.path.join(rsrc, 'graphics.rs')
    if os.path.exists(gfx):
        files.append(gfx)
        src = open(gfx, encoding='utf-8', errors='replace').read()
        m = re.search(r'D3D_BOOTSTRAP\s*:\s*&\[\(&str,\s*&str\)\]\s*=\s*&\[(.*?)\];',
                      src, re.S)
        if m:
            for dll, func in _ARM_LITERAL.findall(m.group(1)):
                implemented.add((dll.lower(), func))

    steam = os.path.join(rsrc, 'steam.rs')
    if os.path.exists(steam):
        files.append(steam)
        src = open(steam, encoding='utf-8', errors='replace').read()
        # Split into the three helper fns and record their literal names.
        # A match arm may list several names joined by ``|`` before ``=>``,
        # so take every quoted string on the left of the arrow.
        fns = re.split(r'\n(?:pub )?fn\s+', src)
        for chunk in fns:
            m = re.match(r'(\w+)', chunk)
            if not m:
                continue
            name = m.group(1)
            if name not in ('steam_api64', 'sce', 'xinput'):
                continue
            names = set()
            for line in chunk.splitlines():
                if '=>' in line:
                    names.update(re.findall(r'"([^"]+)"', line.split('=>')[0]))
            stubs[name] = names

    return implemented, wildcard_dlls, stubs, files


def load_implemented(root):
    implemented, wildcard, stubs, files = parse_runtime(root)
    def is_impl(dll, func):
        d = dll.lower()
        if (d, func) in implemented:
            return True
        if d in wildcard:
            return True
        return False
    return is_impl, wildcard, stubs, files


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------

def human_table(imports, is_impl):
    by_dll = collections.OrderedDict()
    for imp in imports:
        by_dll.setdefault(imp['dll'], []).append(imp)
    lines = []
    lines.append("%-34s %6s %10s" % ("DLL", "FUNCS", "IMPL"))
    lines.append("-" * 54)
    total = 0
    for dll, imps in by_dll.items():
        impl = sum(1 for i in imps if is_impl(i['dll'], i['func'])) if is_impl else 0
        total += len(imps)
        mark = "" if not is_impl else "  %d/%d" % (impl, len(imps))
        lines.append("%-34s %6d%s" % (dll, len(imps), mark))
    lines.append("-" * 54)
    lines.append("%-34s %6d" % ("TOTAL", total))
    lines.append("")
    for dll, imps in by_dll.items():
        lines.append("%s (%d):" % (dll, len(imps)))
        for i in imps:
            if is_impl:
                flag = "impl" if is_impl(dll, i['func']) else "MISSING"
                lines.append("    %-56s  %s" % (i['func'], flag))
            else:
                lines.append("    %s" % i['func'])
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('exe')
    ap.add_argument('--json', action='store_true', help='machine-readable output')
    ap.add_argument('--coverage', action='store_true',
                    help='cross-check against the runtime implementation')
    ap.add_argument('--root', default=None,
                    help='repo root (default: parent of tools/)')
    args = ap.parse_args()

    pe = PE(args.exe)
    imports = pe.imports()

    root = args.root or os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    is_impl = None
    wildcard = set()
    stubs = {}
    files = []
    if args.coverage:
        is_impl, wildcard, stubs, files = load_implemented(root)

    if args.json:
        by_dll = collections.OrderedDict()
        for imp in imports:
            rec = {'func': imp['func']}
            if imp['ordinal'] is not None:
                rec['ordinal'] = imp['ordinal']
            if is_impl is not None:
                rec['implemented'] = bool(is_impl(imp['dll'], imp['func']))
            by_dll.setdefault(imp['dll'], []).append(rec)
        doc = {
            'exe': args.exe,
            'image_base': pe.image_base,
            'entry_rva': pe.entry_rva,
            'total_imports': len(imports),
            'dll_count': len(by_dll),
            'dlls': by_dll,
            'imports': imports,
        }
        if is_impl is not None:
            impl_count = sum(1 for i in imports if is_impl(i['dll'], i['func']))
            doc['implemented'] = impl_count
            doc['missing'] = len(imports) - impl_count
            doc['runtime_wildcard_dlls'] = sorted(wildcard)
            doc['runtime_unwired_stubs'] = {k: sorted(v) for k, v in stubs.items()}
            doc['runtime_sources'] = files
        print(json.dumps(doc, indent=2))
        return

    print(human_table(imports, is_impl))
    if is_impl is not None:
        impl_count = sum(1 for i in imports if is_impl(i['dll'], i['func']))
        print("\nTOTAL %d imports across %d DLLs; implemented %d, missing %d"
              % (len(imports), len(collections.OrderedDict((i['dll'], 1) for i in imports)),
                 impl_count, len(imports) - impl_count))
        print("runtime wildcard DLLs: %s" % ", ".join(sorted(wildcard)) or "(none)")
        for name, names in stubs.items():
            print("runtime stub helper %s() (not wired into ImportResolver::call): %d names"
                  % (name, len(names)))


if __name__ == '__main__':
    main()
