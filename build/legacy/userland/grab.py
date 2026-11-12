#!/usr/bin/env python3
# grab.py <release>...: copy what the userland audit reads out of Apple's recovery image of each release, then detach.
# The image is attached read-only and without browsing. Environment: AUDIT_BASE = folder holding recovery/<release>/*.dmg,
# OUT = where the copies go (OUT/sys/<release>/).
import os, sys, glob, subprocess, plistlib, shutil
BASE = os.environ["AUDIT_BASE"]; OUT = os.environ["OUT"]

def run(cmd, timeout=900, inp="Y\n"):
    p = subprocess.run(cmd, capture_output=True, text=True, errors="replace", timeout=timeout, input=inp, env=dict(os.environ, PAGER="cat"))
    return p.returncode, p.stdout, p.stderr

def attach(dmg):
    rc, out, err = run(["hdiutil", "attach", "-nobrowse", "-readonly", "-noverify", "-plist", dmg])
    if rc != 0: return None, err.strip()[:200]
    pl = plistlib.loads(out[out.index("<?xml"):].encode())
    mps = [e["mount-point"] for e in pl["system-entities"] if "mount-point" in e]
    return (mps, None) if mps else (None, "no mount point")

def detach(mp): run(["hdiutil", "detach", "-force", mp])

def find_root(mps):
    for mp in mps:
        if os.path.isdir(mp + "/System/Library/Extensions"): return mp, []
    extra = []
    for mp in mps:
        for inner in glob.glob(mp + "/**/BaseSystem.dmg", recursive=True) + glob.glob(mp + "/.*/BaseSystem.dmg"):
            m2, err = attach(inner)
            if m2:
                extra += m2
                for x in m2:
                    if os.path.isdir(x + "/System/Library/Extensions"): return x, extra
    return None, extra

WANT_FW = ["Metal", "MetalFX", "Foundation", "CoreFoundation", "IOKit", "IOSurface", "QuartzCore", "CoreGraphics", "OpenGL", "OpenCL",
           "CoreVideo", "VideoToolbox", "VideoDecodeAcceleration", "CoreImage", "CoreMedia", "ApplicationServices", "Security"]
WANT_PFW = ["GPUSupport", "IOAccelerator", "AppleGVA", "GPUWrangler", "IOPresentment", "SkyLight", "CoreDisplay", "MetalTools", "GPUCompiler",
            "IOAccelMemoryInfo", "DisplayServices", "GPUInfo", "AppleVA", "VideoProcessing", "CoreUI", "IOSurfaceAccelerator", "AGXCompilerConnection",
            "MTLCompiler", "GraphicsServices", "CoreBrightness", "MonitorPanel", "SystemUIPlugin"]
EXT_PAT = ["*MTLDriver.bundle", "*GLDriver.bundle", "*VADriver.bundle", "*VADriver2.bundle", "AppleGraphicsControl.kext", "AppleGraphicsPowerManagement.kext",
           "IOAcceleratorFamily2.kext", "IOAcceleratorFamily.kext", "IOGraphicsFamily.kext", "IONDRVSupport.kext", "AppleMCCSControl.kext",
           "AppleBacklight*.kext", "GeForce*.kext", "NVDA*.kext", "AMD*.kext", "ATI*.kext", "AppleIntel*Graphics*.kext", "AppleIntelHD*.kext",
           "AppleIntel*FB*.kext", "AppleIntelFramebuffer*.kext", "AppleGPUWrangler.kext", "AppleUpstreamUserClient.kext", "IOSurface.kext",
           "AppleIntel*VA*.bundle", "*GLDriver*.plugin", "AppleGFXHDA.kext", "AppleHDA.kext/Contents/PlugIns/AppleGFXHDA*", "*OpenCL*", "AppleCameraInterface.kext",
           "IOVideoFamily.kext", "AppleParavirtGPU.kext", "*MetalOpenGL*", "IOGPUFamily.kext", "DisplayPolicy*", "AppleDisplay*.kext", "AppleOnboardDisplay.kext"]

def cp(src, dst):
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    rc, o, e = run(["/bin/cp", "-Rp", src, dst])
    return rc == 0

def grab(name):
    d = f"{BASE}/recovery/{name}"; o = f"{OUT}/sys/{name}"; os.makedirs(o, exist_ok=True)
    dmgs = sorted(glob.glob(d + "/*.dmg"), key=os.path.getsize, reverse=True)
    if not dmgs: return "no image"
    mps, err = attach(dmgs[0])
    if not mps: return "attach failed: " + str(err)
    extra = []
    try:
        R, extra = find_root(mps)
        if not R: return "no system root"
        cp(R + "/System/Library/CoreServices/SystemVersion.plist", o + "/SystemVersion.plist")
        with open(o + "/files.txt", "w") as f:
            n = 0
            for dp, dn, fn in os.walk(R):
                rel = dp[len(R):] or "/"
                for x in dn: f.write(rel.rstrip("/") + "/" + x + "/\n")
                for x in fn:
                    p = os.path.join(dp, x)
                    try: sz = os.lstat(p).st_size
                    except OSError: sz = -1
                    f.write(f"{rel.rstrip('/')}/{x}\t{sz}\n"); n += 1
        got = []
        for fw in WANT_FW:
            s = f"{R}/System/Library/Frameworks/{fw}.framework"
            if os.path.isdir(s) and cp(s, f"{o}/System/Library/Frameworks/{fw}.framework"): got.append(fw)
        for fw in WANT_PFW:
            s = f"{R}/System/Library/PrivateFrameworks/{fw}.framework"
            if os.path.isdir(s) and cp(s, f"{o}/System/Library/PrivateFrameworks/{fw}.framework"): got.append("P:" + fw)
        for pat in EXT_PAT:
            for s in glob.glob(f"{R}/System/Library/Extensions/{pat}"):
                if cp(s, f"{o}/System/Library/Extensions/{s.split('/System/Library/Extensions/')[1]}"): got.append("E:" + os.path.basename(s))
        # every Info.plist of every extension (small), for personalities and bundle keys
        for s in glob.glob(f"{R}/System/Library/Extensions/*/Contents/Info.plist") + glob.glob(f"{R}/System/Library/Extensions/*/Info.plist") + \
                 glob.glob(f"{R}/System/Library/Extensions/*/Contents/PlugIns/*/Contents/Info.plist"):
            dst = f"{o}/plists/" + s.split("/System/Library/Extensions/")[1]
            if not os.path.exists(dst): cp(s, dst)
        for s in ["/usr/lib/system", "/usr/lib/libSystem.B.dylib", "/usr/lib/libc++.1.dylib", "/usr/lib/libc++abi.dylib", "/usr/lib/libobjc.A.dylib",
                  "/usr/lib/libiconv.2.dylib", "/usr/lib/libz.1.dylib", "/usr/lib/libcharset.1.dylib", "/usr/lib/dyld", "/usr/lib/libstdc++.6.dylib",
                  "/usr/lib/libresolv.9.dylib", "/usr/lib/libxml2.2.dylib", "/usr/lib/libexpat.1.dylib",
                  "/System/Library/CoreServices/WindowServer", "/System/Library/Displays", "/System/Library/Extensions/AppleGraphicsControl.kext",
                  "/System/Library/Video", "/System/Library/Compositions", "/System/Library/SystemProfiler/SPDisplaysReporter.spreporter",
                  "/System/Library/CoreServices/Resources/WindowServer", "/System/Library/Kernels/kernel", "/mach_kernel"]:
            if os.path.exists(R + s) and cp(R + s, o + s): got.append(s)
        open(o + "/grabbed.txt", "w").write("\n".join(got) + "\n")
        return f"ok files={n} grabbed={len(got)}"
    finally:
        for mp in extra + mps: detach(mp)

for n in sys.argv[1:]:
    try: r = grab(n)
    except Exception as e: r = "EXC " + repr(e)[:300]
    print(n, r, flush=True)
