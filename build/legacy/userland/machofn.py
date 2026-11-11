#!/usr/bin/env python3
# machofn.py <binary> <otool -tV listing> <regex of function labels> [--all]: print the calls, selectors and string
# constants of the matching functions, with constant-string references resolved from the binary itself.
# machofn.py <binary> <listing> --who <text>: list the functions that reference a constant string containing <text>.
import sys, re, struct
def load(path):
    b = open(path, "rb").read()
    off = 0
    magic = struct.unpack(">I", b[:4])[0]
    if magic in (0xcafebabe, 0xcafebabf):
        n = struct.unpack(">I", b[4:8])[0]
        for i in range(n):
            if magic == 0xcafebabe: ct, cs, o, sz, al = struct.unpack(">IIIII", b[8 + 20 * i: 28 + 20 * i])
            else: ct, cs, o, sz, al, _ = struct.unpack(">IIQQII", b[8 + 32 * i: 40 + 32 * i])
            if ct == 0x01000007: off = o; break
    assert struct.unpack("<I", b[off:off + 4])[0] == 0xfeedfacf, "not a 64-bit Mach-O slice"
    ncmds = struct.unpack("<I", b[off + 16: off + 20])[0]
    p = off + 32; secs = []
    for _ in range(ncmds):
        cmd, sz = struct.unpack("<II", b[p:p + 8])
        if cmd == 0x19:
            nsect = struct.unpack("<I", b[p + 64:p + 68])[0]; q = p + 72
            for _ in range(nsect):
                sn = b[q:q + 16].rstrip(b"\0").decode(); sg = b[q + 16:q + 32].rstrip(b"\0").decode()
                addr, size, fo = struct.unpack("<QQI", b[q + 32:q + 52]); secs.append((sg, sn, addr, size, fo)); q += 80
        p += sz
    return b, off, secs
class Img:
    def __init__(s, path): s.b, s.off, s.secs = load(path)
    def rd(s, vm, n):
        for sg, sn, a, sz, fo in s.secs:
            if a <= vm < a + sz and fo: return s.b[s.off + fo + (vm - a): s.off + fo + (vm - a) + n]
        return None
    def ptr(s, vm):
        d = s.rd(vm, 8)
        if d is None: return None
        v = struct.unpack("<Q", d)[0]
        if v >> 36: v &= (1 << 36) - 1          # a chained-fixup rebase: the target is the low 36 bits
        return v
    def cstr(s, vm):
        out = b""
        while True:
            d = s.rd(vm + len(out), 64)
            if d is None: return None
            i = d.find(b"\0")
            if i >= 0: return (out + d[:i]).decode("utf-8", "replace")
            out += d
    def cfstring(s, vm):
        p = s.ptr(vm + 16)
        if p is None: return None
        fl = s.rd(vm + 8, 8); n = s.rd(vm + 24, 8)
        if fl and struct.unpack("<Q", fl)[0] & 0x10 and n:       # UTF-16 backing store
            d = s.rd(p, struct.unpack("<Q", n)[0] * 2); return d.decode("utf-16le", "replace") if d else None
        return s.cstr(p)
    def insec(s, vm, name):
        return any(sn == name and a <= vm < a + sz for sg, sn, a, sz, fo in s.secs)
def functions(listing):
    cur = None; body = []
    for l in open(listing, errors="replace"):
        l = l.rstrip("\n")
        if l.endswith(":") and not l[:1].isdigit() and not l.startswith("("):
            if cur: yield cur, body
            cur = l[:-1]; body = []
        elif cur is not None and l[:1].isdigit(): body.append(l)
    if cur: yield cur, body
def notes(img, body):
    out = []
    for i, l in enumerate(body):
        m = re.match(r"^([0-9a-f]+)\s+(.*)$", l)
        if not m: continue
        ins = m.group(2); txt = None
        r = re.search(r"(-?0x[0-9a-f]+)\(%rip\)", ins)
        if r and i + 1 < len(body):
            nxt = re.match(r"^([0-9a-f]+)", body[i + 1])
            if nxt:
                tgt = (int(nxt.group(1), 16) + int(r.group(1), 16)) & 0xffffffffffffffff
                if img.insec(tgt, "__cfstring"): txt = 'CFSTR("%s")' % img.cfstring(tgt)
        c = ins.split("##", 1)[1].strip() if "##" in ins else None
        if txt: out.append(txt)
        elif c and "bad cfstring" not in c: out.append(c)
        elif "callq" in ins and not c: out.append("call " + ins.split()[-1])
    return out
if __name__ == "__main__":
    img = Img(sys.argv[1])
    if sys.argv[3] == "--who":
        for name, body in functions(sys.argv[2]):
            hit = [n for n in notes(img, body) if n.startswith("CFSTR") and sys.argv[4] in n]
            if hit: print(name, "|", "; ".join(sorted(set(hit))))
    else:
        rx = re.compile(sys.argv[3])
        for name, body in functions(sys.argv[2]):
            if rx.search(name):
                print("==", name, "(%d instructions)" % len(body))
                for n in notes(img, body): print("   ", n[:170])
