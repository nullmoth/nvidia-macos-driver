#!/usr/bin/env python3
"""Resolve GPU fault addresses against the fault file the plugin writes at device loss ($TMPDIR/nvmtl-fault-<pid>.txt).

usage: fault_resolve.py <nvmtl-fault-PID.txt> <address> [<address> ...]

Each address is reported as the live range that holds it (kind, range, size, the submit it was made at), else the
recorded death that held it, else "never mapped by this process's buffers or pages (an image, NVK's own memory, or a computed pointer)" - an image, NVK's own memory, or a pointer the kernel
computed. The dispatch roots the file holds (every binding-table buffer address of the dying command buffer's last
dispatches, already resolved by the plugin) are printed after."""
import sys

def main():
    if len(sys.argv) < 3: print(__doc__); return 2
    live, dead, pages, roots, was, mems, imgs, tics = [], [], [], [], [], [], [], []
    skipped = 0
    def parse(ln):
        w = ln.split()
        if not w: return
        if w[0] == "live":            # live <kind words...> 0xlo 0xhi bytes seq S
            i = next(k for k, x in enumerate(w) if x.startswith("0x")); live.append((int(w[i], 16), int(w[i + 1], 16), " ".join(w[1:i]), int(w[-1])))
        elif w[0] == "was":           # was <kind words...> 0xlo 0xhi bytes seq S died D
            i = next(k for k, x in enumerate(w) if x.startswith("0x")); was.append((int(w[i], 16), int(w[i + 1], 16), " ".join(w[1:i]), int(w[i + 4]), int(w[i + 6])))
        elif w[0] == "dead":          # dead buffer 0xlo 0xhi bytes seq S [flags]
            dead.append((int(w[2], 16), int(w[3], 16), int(w[6]), " ".join(w[7:])))
        elif w[0] == "page":          # page own|cooling|cached 0xlo 0xhi bytes seq S
            pages.append((int(w[2], 16), int(w[3], 16), w[1], int(w[-1])))
        elif w[0] == "root": roots.append(ln.rstrip())
        elif w[0] == "tic":           # tic view V image I sidx N saddr 0x.. tidx M taddr 0x.. type T layers b+n mips b+n seq A died B
            tics.append((w[2], w[4], int(w[6]), int(w[8], 16), int(w[10]), int(w[12], 16), int(w[w.index("seq") + 1]), int(w[w.index("died") + 1])))
        elif w[0] == "memory" and "mem" in w:   # memory <kind> mem 0x.. base 0x.. size S type T seq A died B
            i = w.index("mem"); mems.append((w[i + 1], int(w[i + 3], 16), int(w[i + 5]), " ".join(w[1:i]), int(w[i + 9]), int(w[i + 11])))
        elif w[0] == "image" and "mem" in w and "off" in w:   # image <img> view <v> mem <m> off <o> size <s> WxH fmt F ... seq C died D
            imgs.append((w[1], w[w.index("mem") + 1], int(w[w.index("off") + 1]), int(w[w.index("size") + 1]), w[w.index("size") + 2], w[w.index("fmt") + 1], int(w[w.index("seq") + 1]), int(w[w.index("died") + 1])))
    for ln in open(sys.argv[1]):
        try: parse(ln)
        except (StopIteration, ValueError, IndexError): skipped += 1   # a torn or foreign line (the dump is written while the process dies) is skipped, never fatal
    if skipped: print(f"{skipped} malformed line(s) skipped")
    print(f"{len(live)} live ranges, {len(was)} ranges that once lived, {len(dead)} recorded deaths, {len(pages)} pages, {len(roots)} dispatch roots, {len(mems)} memories, {len(imgs)} images, {len(tics)} views with headers")
    isize = {}
    for im in imgs: isize[im[0]] = max(isize.get(im[0], 0), im[3])
    rc = 0
    for a in sys.argv[2:]:
        addr = int(a, 16)
        hit = [(lo, hi, k, s) for lo, hi, k, s in pages if lo <= addr < hi]
        if hit: lo, hi, k, s = hit[0]; print(f"0x{addr:x} lies in ADDRESS PAGE ({k}) 0x{lo:x}..0x{hi:x} last referenced by submit {s}"); continue
        hit = [(lo, hi, k, s) for lo, hi, k, s in live if lo <= addr < hi]
        if hit: lo, hi, k, s = hit[0]; print(f"0x{addr:x} lies in LIVE {k} 0x{lo:x}..0x{hi:x} ({hi - lo} bytes), alive since submit {s}"); continue
        hit = [(lo, hi, s, f) for lo, hi, s, f in dead if lo <= addr < hi]
        if hit: lo, hi, s, f = hit[0]; print(f"0x{addr:x} lies in DEAD buffer 0x{lo:x}..0x{hi:x} ({hi - lo} bytes{', ' + f if f else ''}) freed after submit {s}"); continue
        hit = [(lo, hi, k, s, d) for lo, hi, k, s, d in was if lo <= addr < hi]
        if hit: lo, hi, k, s, d = hit[-1]; print(f"0x{addr:x} WAS {k} 0x{lo:x}..0x{hi:x} ({hi - lo} bytes), created at submit {s}, died at submit {d} (newest of {len(hit)} that held it)"); continue
        # the image headers: where the GPU was told each view's image lives (NVK binds images at their own addresses)
        th = []
        for v, i, sidx, saddr, tidx, taddr, c, d in tics:
            sz = isize.get(i, 0) or (64 << 20)
            for role, idx, a in (("storage", tidx, taddr), ("sampled", sidx, saddr)):
                if a and a <= addr < a + sz: th.append((c, v, i, role, idx, a, d))
        if th:
            th.sort(); c, v, i, role, idx, a, d = th[-1]
            dims = next((im[4] for im in imgs if im[0] == i), "")
            try: width = int(dims.split("x")[0])
            except Exception: width = 0
            rowhint = f"; with a {width}-wide 4-byte-per-texel row of {width * 4} B that is tile row {(addr - a) // (width * 4)}" if width else ""
            print(f"0x{addr:x} = image {i} by its {role} header (view {v}, descriptor index {idx}, header address 0x{a:x}, +{addr - a} into it{rowhint}; image size {isize.get(i, 0) or 'unknown'}), view created seq {c}, {'DIED seq ' + str(d) if d else 'alive'}; {len(th)} header(s) covered it: " + ", ".join(f"{vv} idx {ii} seq {cc}..{dd or 'alive'}" for cc, vv, _, _, ii, _, dd in th)); rc = 1; continue
        hit = [m for m in mems if m[1] and m[1] <= addr < m[1] + m[2]]
        if hit:
            mem, base, size, kind, c, d = hit[-1]; off = addr - base
            cands = [im for im in imgs if im[1] == mem and im[2] <= off]
            best = max(cands, key=lambda im: (im[2], im[6])) if cands else None
            tail = f" -> image {best[0]} ({best[4]} fmt {best[5]}, created seq {best[6]}, {'died seq ' + str(best[7]) if best[7] else 'alive'}) bound at offset {best[2]} (+{off - best[2]} into it)" if best else " -> no image bound at or below that offset in the table"
            print(f"0x{addr:x} = {kind} memory {mem} (base 0x{base:x}, size {size}, created seq {c}, {'DIED at seq ' + str(d) if d else 'ALIVE'}) + offset {off}{tail}"); rc = 1; continue
        near = sorted(live, key=lambda e: min(abs(e[0] - addr), abs(e[1] - addr)))[:2]
        print(f"0x{addr:x} is NEVER MAPPED by this plugin's buffers or pages (an image, NVK's own memory, or a computed pointer); nearest live: "
              + "; ".join(f"{k} 0x{lo:x}..0x{hi:x}" for lo, hi, k, s in near)); rc = 1
    for r in roots: print(r)
    return rc

def selftest():
    import tempfile, io, contextlib
    f = tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False)
    f.write("live buffer 0x36f100000 0x36f140000 262144 seq 7000\nlive buffer\nlive garbage without words\nwas placed buffer 0x2ef800000 0x2efa00000\ndead buffer 0x2ef8a0000 0x2ef8b0000 65536 seq 7900 pooled\ntic view 0xa2 image 0xa0 sidx 11 saddr 0x2ef400000 tidx 10263\n")
    f.close()
    sys.argv = [sys.argv[0], f.name, "0x36f122000", "0x2ef8a2000"]
    out = io.StringIO()
    with contextlib.redirect_stdout(out): rc = main()
    s = out.getvalue(); fails = 0
    def arm(name, ok):
        nonlocal fails
        print(("ok    " if ok else "FAIL  ") + name); fails += 0 if ok else 1
    arm("malformed lines are counted and skipped, never a crash", "4 malformed line(s) skipped" in s)
    arm("the well-formed lines still resolve", "0x36f122000 lies in LIVE buffer 0x36f100000..0x36f140000" in s and "0x2ef8a2000 lies in DEAD buffer 0x2ef8a0000..0x2ef8b0000" in s)
    print(f"fault_resolve selftest: {fails} failed"); return 1 if fails else 0

if __name__ == "__main__":
    sys.exit(selftest() if len(sys.argv) > 1 and sys.argv[1] == "--selftest" else main())
