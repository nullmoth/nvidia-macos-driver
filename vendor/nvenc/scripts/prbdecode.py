#!/usr/bin/env python3
# prbdecode.py: decode RM's protobuf debug records (e.g. the RC error records tools/rcerr prints) using the field
# descriptors in open-gpu-kernel-modules' generated g_*_pb.c / g_*_pb.h.
#   ssh tahoe 'sudo ~/nvenc-dev/rcerr 1' | scripts/prbdecode.py [--type Rc.RcDiagRecord] [--skip 8]
# Input: rcerr's output (records as little-endian 32-bit hex words). Unknown fields print as raw tags.
import glob, os, re, sys

GEN = os.path.join(os.path.dirname(__file__), '..', 'vendor', 'open-gpu-kernel-modules', 'src', 'nvidia', 'generated')

def load_schema():
    msgs, macro, enums = {}, {}, {}   # msgs: name -> {num: (label, type, fname, submsg-macro, enum-desc)}
    for h in glob.glob(os.path.join(GEN, 'g_*_pb.h')):
        for m in re.finditer(r'#define\s+(\w+)\s+\(&prb_messages_(\w+)\[(\d+)\]\)', open(h).read()):
            macro[m.group(1)] = (m.group(2), int(m.group(3)))
    tables = {}
    for c in glob.glob(os.path.join(GEN, 'g_*_pb.c')):
        s = open(c).read()
        for m in re.finditer(r'static const PRB_ENUM_MAPPING (prb_enum_mappings_\w+)\[\] = \{(.*?)\n\};', s, re.S):
            enums[m.group(1)] = {int(v): n for v, n in re.findall(r'\{\s*(-?\d+),\s*PRB_MAYBE_ENUM_NAME\("(\w+)"\)', m.group(2))}
        for m in re.finditer(r'const PRB_ENUM_DESC (prb_enums_\w+) = \{\s*(prb_enum_mappings_\w+)', s):
            enums[m.group(1)] = enums.get(m.group(2), {})
        for m in re.finditer(r'const PRB_FIELD_DESC (prb_fields_\w+)\[\] = \{(.*?)\n\};', s, re.S):
            fields = {}
            for f in re.finditer(r'\{\s*(\d+),\s*\{\s*(PRB_\w+),\s*(PRB_\w+),[^}]*\},\s*([\w&]+),\s*([\w&]+),\s*PRB_MAYBE_FIELD_NAME\("(\w+)"\)', m.group(2)):
                fields[int(f.group(1))] = (f.group(2), f.group(3), f.group(6), f.group(4), f.group(5).lstrip('&'))
            tables[m.group(1)] = fields
        for m in re.finditer(r'const PRB_MSG_DESC prb_messages_(\w+)\[\] = \{(.*?)\n\};', s, re.S):
            for i, e in enumerate(re.findall(r'\{\s*\d+,\s*(prb_fields_\w+),\s*PRB_MAYBE_MESSAGE_NAME\("([\w.]+)"\)', m.group(2))):
                msgs[(m.group(1), i)] = (e[1], e[0])
    byname = {n: tables.get(t, {}) for (_, n, t) in [(k, v[0], v[1]) for k, v in msgs.items()]}
    resolve = lambda mac: msgs.get(macro.get(mac, (None, None)), (None,))[0]
    return byname, resolve, enums

def varint(b, i):
    v = s = 0
    while True:
        x = b[i]; i += 1; v |= (x & 0x7f) << s; s += 7
        if x < 0x80: return v, i

def regs(b):
    """Parse b strictly as Regs.RegsAndMem (type=1, offset=2, stride=3, val=4 packed); None if it isn't one."""
    try:
        i, off, stride, vals, seen = 0, None, 0, [], set()
        while i < len(b):
            tag, i = varint(b, i); num, wt = tag >> 3, tag & 7
            if num in (1, 2, 3) and wt == 0:
                v, i = varint(b, i); seen.add(num)
                if num == 2: off = v
                if num == 3: stride = v
            elif num == 4 and wt == 2:
                n, i = varint(b, i); e = i + n
                while i < e: v, i = varint(b, i); vals.append(v)
                if i != e: return None
            elif num == 4 and wt == 0:
                v, i = varint(b, i); vals.append(v)
            else:
                return None
        return (off, stride, vals) if off is not None and 1 in seen and i == len(b) else None
    except IndexError:
        return None

def decode(b, typ, schema, depth=0, out=print):
    byname, resolve, enums = schema
    fields = byname.get(typ, {}) if typ else {}
    i, pad = 0, '  ' * depth
    while i < len(b):
        try:
            i = field(b, i, fields, pad, schema, depth, out)
        except IndexError:
            out(f'{pad}(truncated)'); return
        if i is None: return

def field(b, i, fields, pad, schema, depth, out):
        byname, resolve, enums = schema
        tag, i = varint(b, i)
        num, wt = tag >> 3, tag & 7
        lab, ft, name, sub, en = fields.get(num, (None, None, f'#{num}', '0', '0'))
        if wt == 0:
            v, i = varint(b, i)
            ename = enums.get(en, {}).get(v) if ft == 'PRB_ENUM' else None
            out(f'{pad}{name} = {v} (0x{v:x}){" " + ename if ename else ""}')
        elif wt == 1:
            out(f'{pad}{name} = 0x{int.from_bytes(b[i:i+8], "little"):x}'); i += 8
        elif wt == 5:
            out(f'{pad}{name} = 0x{int.from_bytes(b[i:i+4], "little"):x}'); i += 4
        elif wt == 2:
            n, i = varint(b, i); chunk = b[i:i + n]; i += n
            subtype = resolve(sub) if ft == 'PRB_MESSAGE' else None
            if ft in ('PRB_STRING',):
                out(f'{pad}{name} = "{chunk.decode(errors="replace")}"')
            elif ft == 'PRB_BYTES':
                out(f'{pad}{name} = bytes[{n}] {chunk[:64].hex()}')
            elif (subtype in (None, 'Regs.RegsAndMem')) and regs(chunk) is not None:
                off, stride, vals = regs(chunk)
                out(f'{pad}{name}: reg 0x{off:06x}{f" stride {stride}" if stride else ""} = {" ".join(f"{v:08x}" for v in vals)}')
            elif subtype or ft is None:
                out(f'{pad}{name} {{ ({subtype or "?"}, {n} bytes)'); decode(chunk, subtype, schema, depth + 1, out); out(f'{pad}}}')
            else:   # packed repeated scalars
                vals, j = [], 0
                while j < n: v, j = varint(chunk, j); vals.append(hex(v))
                out(f'{pad}{name} = [{", ".join(vals)}]')
        else:
            out(f'{pad}(wire type {wt} at {i}: stop)'); return None
        return i

def main():
    a = sys.argv[1:]
    typ = a[a.index('--type') + 1] if '--type' in a else None
    skip = int(a[a.index('--skip') + 1]) if '--skip' in a else 8
    schema = load_schema()
    if '--list' in a:
        print('\n'.join(sorted(schema[0]))); return
    recs, cur, size = [], None, 0
    for line in sys.stdin:
        m = re.match(r'record (\d+) -> 0x0, (\d+) bytes', line)
        if m: cur = bytearray(); size = int(m.group(2)); recs.append((m.group(1), cur, size)); continue
        if cur is not None and re.match(r'^  [0-9a-f]{8}', line):
            for w in line.split(): cur += int(w, 16).to_bytes(4, 'little')
    for n, b, size in recs:
        b = bytes(b[:size])
        print(f'== record {n}: {size} bytes, header {b[:skip].hex()}')
        decode(b[skip:], typ, schema)

main()
