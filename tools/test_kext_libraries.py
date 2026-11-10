#!/usr/bin/env python3
"""build/check_kext_libraries.sh: a kext that imports from an IOKit family must list that family in OSBundleLibraries.

The keyboard backlight plugin subclasses IOHIDDevice and its Info.plist had no IOHIDFamily entry: it compiled and linked,
and the kernel linker would have left sixty symbols unresolved on every machine. The check reads the import table, so the
test builds small objects that really import a family's symbols. Needs clang (x86_64 objects) and plutil."""
import plistlib, shutil, subprocess, sys, tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHECK = ROOT / "build/check_kext_libraries.sh"
fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail.strip()[-200:]})" if detail and not ok else ""))
    fails += not ok


def kext(t, name, cls, libs):
    """<t>/<name>.kext whose binary imports <cls>::gMetaClass and whose plist lists `libs`."""
    k = t / f"{name}.kext/Contents"; (k / "MacOS").mkdir(parents=True)
    src = t / f"{name}.cpp"
    src.write_text(f"class {cls} {{ public: static void *gMetaClass; }};\nvoid *use() {{ return {cls}::gMetaClass; }}\n" if cls
                   else "extern \"C\" void *IOMalloc(unsigned long); void *use() { return IOMalloc(1); }\n")
    subprocess.run(["xcrun", "clang++", "-arch", "x86_64", "-c", str(src), "-o", str(k / "MacOS" / name)], check=True, capture_output=True)
    (k / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": f"t.{name}", "OSBundleLibraries": {l: "1.0" for l in libs}}))
    return t / f"{name}.kext"


def check(*kexts):
    r = subprocess.run(["bash", str(CHECK)] + [str(k) for k in kexts], capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr


if not (shutil.which("xcrun") and shutil.which("plutil")):
    print("SKIP: needs clang and plutil"); sys.exit(2)
with tempfile.TemporaryDirectory() as d:
    t = Path(d)
    rc, out = check(kext(t, "HidNoLib", "IOHIDDevice", ["com.apple.kpi.iokit"]))
    arm("a kext that imports IOHIDDevice without listing IOHIDFamily is refused and the library is named",
        rc != 0 and "com.apple.iokit.IOHIDFamily" in out and "HidNoLib" in out, out)
    rc, out = check(kext(t, "HidLib", "IOHIDDevice", ["com.apple.kpi.iokit", "com.apple.iokit.IOHIDFamily"]))
    arm("the same kext with the entry passes", rc == 0, out)
    rc, out = check(kext(t, "Plain", None, ["com.apple.kpi.iokit"]))
    arm("a kext that imports only kernel interfaces needs no family entry", rc == 0, out)
    rc, out = check(kext(t, "Acpi", "IOACPIPlatformDevice", ["com.apple.kpi.iokit"]))
    arm("the ACPI family is checked the same way", rc != 0 and "com.apple.iokit.IOACPIFamily" in out, out)
    rc, out = check(kext(t, "Accel", "IOGraphicsAccelerator2", ["com.apple.iokit.IOAcceleratorFamily2"]))
    arm("an accelerator is satisfied by the accelerator family", rc == 0, out)
    rc, out = check(t / "HidLib.kext", t / "HidNoLib.kext")
    arm("one bad kext among several fails the run", rc != 0, out)
    (t / "Empty.kext/Contents/MacOS").mkdir(parents=True)
    rc, out = check(t / "Empty.kext")
    arm("a folder that is not a complete kext is refused, not passed", rc != 0 and "not a complete kext" in out, out)
plist = plistlib.loads((ROOT / "kexts/NMKbdBacklight/Info.plist").read_bytes())
arm("the keyboard backlight plugin lists IOHIDFamily", "com.apple.iokit.IOHIDFamily" in plist["OSBundleLibraries"])
arm("its build runs the check", "check_kext_libraries.sh" in (ROOT / "kexts/NMKbdBacklight/build.sh").read_text())
arm("the package gate runs the check on every kext", "check_kext_libraries.sh" in (ROOT / "package/check_package.sh").read_text())
print("FAILED" if fails else "PASS"); sys.exit(1 if fails else 0)
