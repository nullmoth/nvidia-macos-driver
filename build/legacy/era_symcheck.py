#!/usr/bin/env python3
# era_symcheck.py <out dir> <unpacked Kernel Debug Kit of the release> [recovery-image results json]
# Every symbol a linked era kext imports must be defined by that release's kernel or one of Apple's kexts in the kit.
import os, re, subprocess, sys
out, kit = sys.argv[1], sys.argv[2]
def run(c):
    p = subprocess.run(c, capture_output=True, text=True, errors="replace"); return p.returncode, p.stdout
have = set(); nfiles = 0; kern = None
for dp, dn, fn in os.walk(kit):
    if ".dSYM" in dp: continue
    for f in fn:
        p = os.path.join(dp, f)
        is_kernel = f in ("mach_kernel", "kernel") and os.path.getsize(p) > 1_000_000
        is_kext = dp.endswith(".kext/Contents/MacOS") or (dp.endswith(".kext") and f == os.path.basename(dp)[:-5])
        if not (is_kernel or is_kext): continue
        rc, o = run(["nm", "-arch", "x86_64", "-gU", p])
        if rc != 0: continue
        s = {l.split()[-1] for l in o.splitlines() if l.strip()}
        if s: have |= s; nfiles += 1
        if is_kernel and s: kern = p
# Apple's published export lists for the release add what a symbol table cannot show: names the kernel exports as
# aliases of another function (IOLockLock is lck_mtx_lock, __bzero is bzero). Fetched once per kernel tag and kept.
tag = sys.argv[3] if len(sys.argv) > 3 else None; aliases = 0
if tag:
    import urllib.request
    d = os.path.join(os.path.dirname(os.path.abspath(kit.rstrip("/"))), "..", "xnu-exports", tag); os.makedirs(d, exist_ok=True)
    for f in ("BSDKernel", "IOKit", "Libkern", "Mach", "Unsupported", "Private", "MACFramework", "DSEP"):
        for suf in ("", ".x86_64", ".MacOSX", ".x86_64.MacOSX"):
            fp = os.path.join(d, f + suf + ".exports")
            if not os.path.exists(fp):
                try: data = urllib.request.urlopen(f"https://raw.githubusercontent.com/apple-oss-distributions/xnu/{tag}/config/{f}{suf}.exports", timeout=30).read()
                except Exception: data = b""
                open(fp, "wb").write(data)
            for l in open(fp, errors="replace"):
                l = l.split("#")[0].strip()
                if ":" in l and "*" not in l:
                    name, target = l.split(":", 1)
                    if target in have and name not in have: have.add(name); aliases += 1
    print(f"export lists {tag}: {aliases} alias names added (each points at a symbol the kernel defines)")
print(f"kit {kit}: kernel {'found' if kern else 'NOT FOUND'}, {nfiles} binaries, {len(have)} defined symbols")
if not kern or len(have) < 5000: print("STOP: the kit did not yield a kernel symbol table; nothing can be concluded"); sys.exit(2)
bad = 0
for name in sorted(os.listdir(out)):
    if not name.endswith(".imports.txt"): continue
    imp = [l.strip() for l in open(os.path.join(out, name)) if l.strip()]
    miss = sorted(x for x in imp if x not in have)
    k = name[:-12]; open(os.path.join(out, k + ".missing.txt"), "w").write("\n".join(miss) + ("\n" if miss else ""))
    print(f"{k}: {len(imp)} imports, {len(miss)} not defined by this release")
    for m in miss[:40]: print("    " + m)
    bad += len(miss)
sys.exit(1 if bad else 0)
