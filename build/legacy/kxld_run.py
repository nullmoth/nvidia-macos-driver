#!/usr/bin/env python3
# kxld_run.py <release> <kxlink> <folder with the release's kernel and Apple kexts> <folder with our kexts> [more kext folders]
# Link test with the release's own kext linker. Order: Apple's own families first (controls: if the tool cannot link
# Apple's kexts for this release, nothing it says about ours counts), then ours, then a negative control (our
# accelerator without its family, which must be refused).
import os, plistlib, subprocess, sys, tempfile
rel, kxlink, roots, ours = sys.argv[1], sys.argv[2], [sys.argv[3]] + sys.argv[5:], sys.argv[4]
tmp = tempfile.mkdtemp(prefix="kxld-")
def thin(p):
    o = os.path.join(tmp, p.replace("/", "_")[-150:])
    if not os.path.exists(o):
        r = subprocess.run(["lipo", p, "-thin", "x86_64", "-output", o], capture_output=True)
        if r.returncode != 0: subprocess.run(["cp", p, o], check=True)
    return o
def is_macho64(p):
    try: return open(p, "rb").read(4) in (b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf")
    except OSError: return False
kern = None; idx = {}
def scan(root, mine=False):
    global kern
    for dp, dn, fn in os.walk(root):
        if ".dSYM" in dp: dn[:] = []; continue
        for f in fn:
            p = os.path.join(dp, f)
            # the release kernel, not a debug or instrumented build of it
            if not mine and f in ("kernel", "mach_kernel") and os.path.getsize(p) > 1_000_000 and is_macho64(p) \
               and (not kern or ("DEBUG" in kern and "DEBUG" not in p)): kern = p
        if dp.endswith(".kext"):
            for ip in (os.path.join(dp, "Contents", "Info.plist"), os.path.join(dp, "Info.plist")):
                if os.path.exists(ip): break
            else: continue
            try: pl = plistlib.load(open(ip, "rb"))
            except Exception: continue
            bid = pl.get("CFBundleIdentifier"); exe = pl.get("CFBundleExecutable")
            if not bid: continue
            b = None
            if exe:
                for bp in (os.path.join(dp, "Contents", "MacOS", exe), os.path.join(dp, exe)):
                    if os.path.exists(bp): b = bp; break
            if bid in idx and not mine: continue
            idx[bid] = dict(bin=b, libs=dict(pl.get("OSBundleLibraries") or {}), path=dp, mine=mine,
                            iface=bid.startswith("com.apple.kpi.") or bid.startswith("com.apple.kernel"))
for r in roots: scan(r)
scan(ours, mine=True)
if not kern: print(f"{rel}: STOP, no kernel under {roots}"); sys.exit(2)
print(f"{rel}: kernel {kern}; {len(idx)} bundles indexed")
def closure(bid, seen=None):
    seen = seen if seen is not None else []
    for l in idx[bid]["libs"]:
        if l in idx and l not in seen: seen.append(l); closure(l, seen)
    return seen
# A dependency is handed to the linker in linked form, as the kernel does: a child's class table can only be
# patched against a parent table that was itself patched. If a dependency does not link, its file as shipped is
# used and the fact is printed, because results below it are then weaker.
done = {}; addr_of = {}
def linked(bid):
    if bid in done: return done[bid]
    o = os.path.join(tmp, "linked_" + bid)
    rc, out = link(bid, out_path=o)
    if rc == 0 and os.path.exists(o): done[bid] = o
    else:
        done[bid] = thin(idx[bid]["bin"]); print(f"  (dependency {bid} did not link here; using its file as shipped)")
    return done[bid]
# 10.6 form of the linker (KX_STATE=1): a dependency is not a binary but the "link state" an earlier link of that
# bundle wrote, starting from the kernel itself. Each bundle is linked once and its state reused.
STATE = os.environ.get("KX_STATE") == "1"; states = {}
def run_state(bin_path, name, deps, state_out, verbose=False):
    n = addr_of.setdefault(name, len(addr_of) + 1)
    args = [kxlink, "-a", "%x" % (0xffffff7f80000000 + 0x2000000 * n)] + (["-v"] if verbose else []) + (["-s", state_out] if state_out else []) + [bin_path, name]
    for d in deps: args += ["-d", d]
    try: r = subprocess.run(args, capture_output=True, text=True, errors="replace", timeout=300)
    except subprocess.TimeoutExpired: return None, f"{name}: linker did not finish in 300 s"
    return r.returncode, (r.stdout + r.stderr).strip()
def state_of(bid):
    if bid in states: return states[bid]
    o = os.path.join(tmp, "state_" + bid)
    if bid == "__kernel__": rc, out = run_state(thin(kern), "mach_kernel", [], o)
    else:
        e = idx[bid]; deps = [state_of(l) for l in e["libs"] if l in idx and idx[l]["bin"]] or [state_of("__kernel__")]
        rc, out = run_state(thin(e["bin"]), bid, [d for d in deps if d], o)
    states[bid] = o if rc == 0 and os.path.exists(o) else None
    if not states[bid]: print(f"  (no link state for {bid}: {out.splitlines()[-1] if out else 'no output'})")
    return states[bid]
def link_state(bid, drop=None, verbose=False):
    e = idx.get(bid)
    if not e or not e["bin"]: return None, f"{bid}: not present for this release"
    deps = [state_of(l) for l in e["libs"] if l in idx and idx[l]["bin"] and l != drop]
    missing = [l for l in e["libs"] if l not in idx]
    rc, out = run_state(thin(e["bin"]), bid, [d for d in deps if d], None, verbose)
    if missing: out += "\n  (libraries named but not on hand: " + ", ".join(missing) + ")"
    return rc, out
def link(bid, drop=None, verbose=False, out_path=None):
    if STATE: return link_state(bid, drop, verbose)
    e = idx.get(bid)
    if not e or not e["bin"]: return None, f"{bid}: not present for this release"
    # one 32 MB range per bundle inside the 2 GB kext region, so no two linked images share an address and every
    # one stays within reach of the kernel
    n = addr_of.setdefault(bid, len(addr_of) + 1)
    if n > 62: return None, f"{bid}: more than 62 images in one test, address ranges exhausted"
    base = 0xffffff7f80000000 + 0x2000000 * n
    args = [kxlink, "-a", "%x" % base] + (["-v"] if verbose else []) + (["-o", out_path] if out_path else []) + [thin(e["bin"]), bid]; missing = []
    for l in closure(bid):
        if l == drop: continue
        d = idx[l]; flag = "-d" if l in e["libs"] else "-i"
        if d["iface"]:
            if d["bin"]: args += [flag, thin(kern) + "," + thin(d["bin"])]
            else: args += [flag, thin(kern)]
        elif d["bin"]: args += [flag, linked(l)]
    for l in e["libs"]:
        if l not in idx: missing.append(l)
    try: r = subprocess.run(args, capture_output=True, text=True, errors="replace", timeout=300)
    except subprocess.TimeoutExpired: return None, f"{bid}: linker did not finish in 300 s"
    out = (r.stdout + r.stderr).strip()
    if r.returncode != 0 and "[ERROR]" not in out and not verbose:
        # refused without saying why at the error level: ask again with every message and keep the tail
        rc2, out2 = link(bid, drop, True); out = "\n".join(out2.splitlines()[-14:])
    if missing: out += "\n  (libraries named but not on hand: " + ", ".join(missing) + ")"
    return r.returncode, out
controls = ["com.apple.iokit.IOPCIFamily", "com.apple.iokit.IOGraphicsFamily", "com.apple.iokit.IOSurface",
            "com.apple.iokit.IOAcceleratorFamily2", "com.apple.iokit.IONDRVSupport"]
ok_controls = 0
only = os.environ.get("KXLINK_ONLY")     # debugging aid: link one bundle and print everything the linker said
if only:
    rc, out = link(only, verbose=True); print(out[-6000:]); sys.exit(0)
print("-- controls: Apple's own kexts of this release")
for c in controls:
    rc, out = link(c)
    if rc is None: print("  " + out); continue
    lines = out.splitlines(); print("  " + lines[-1] if rc == 0 else "  " + "\n  ".join(lines[-12:]))
    ok_controls += rc == 0
print(f"-- controls linked: {ok_controls}")
print("-- ours")
bad = 0; mine = sorted(b for b, e in idx.items() if e["mine"])
for b in mine:
    rc, out = link(b)
    if rc is None: print("  " + out); continue
    lines = out.splitlines(); print("  " + ("\n  ".join(lines[-25:])))
    bad += rc != 0
acc = next((b for b in mine if b.lower().endswith("nvaccel")), None)
if acc:
    rc, out = link(acc, drop="com.apple.iokit.IOAcceleratorFamily2")
    print("-- negative control (accelerator without its family; must be REFUSED): " + ("fired" if rc == 1 else f"DID NOT FIRE rc={rc}"))
else:
    # releases without our accelerator: the display kext without the graphics family must be refused
    fb = next((b for b in mine if b.lower().endswith("nvrmfb")), None)
    if fb:
        rc, out = link(fb, drop="com.apple.iokit.IOGraphicsFamily")
        print("-- negative control (display kext without the graphics family; must be REFUSED): " + ("fired" if rc == 1 else f"DID NOT FIRE rc={rc}"))
subprocess.run(["rm", "-rf", tmp])
print(f"{rel}: RESULT controls_linked={ok_controls} ours_refused={bad} ours_total={len(mine)}")
