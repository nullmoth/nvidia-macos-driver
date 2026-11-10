#!/usr/bin/env python3
"""The NVIDIA core archives NVRM links must come from a tree that carries exactly this revision's patches.

kexts/NVRM/patches used to be applied by hand to the tree the archives were built from. Two patches (hardware YUV 4:2:0,
the colour-range control) were added to the repository and never applied, so the shipped kernel driver had neither while
every source check passed. build_rm620.sh now applies the chain itself and records the names in the tree, and
build_nvrm.sh refuses a tree whose record differs. The refusal is run here against stand-in trees."""
import pathlib, shutil, subprocess, sys, tempfile

R = pathlib.Path(__file__).resolve().parents[1]
fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({str(detail).strip()[-220:]})" if detail and not ok else ""))
    fails += not ok


names = sorted(p.name for p in (R / "kexts/NVRM/patches").glob("*.patch"))
arm("the chain holds the YUV 4:2:0 and colour-range patches", "620-yuv420-hw.patch" in names and "620-color-range.patch" in names, names)
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)

    def run(record):
        tree = d / "ogkm"; shutil.rmtree(tree, ignore_errors=True); (tree / "src").mkdir(parents=True)
        if record is not None:
            (tree / ".nm-patches").write_text("".join(n + "\n" for n in record))
        r = subprocess.run(["bash", str(R / "build/build_nvrm.sh"), str(R / "kexts/NVRM"), str(tree), str(d / "out")], capture_output=True, text=True)
        return r.returncode, r.stdout + r.stderr

    rc, out = run(None)
    arm("a tree with no record of its patches (patched by hand) is refused", rc == 2 and "was built with patches []" in out, out)
    rc, out = run([n for n in names if n not in ("620-yuv420-hw.patch", "620-color-range.patch")])
    arm("control: the set the shipped archives really had (two patches short) is refused", rc == 2 and "620-yuv420-hw.patch" in out, out)
    rc, out = run(names)
    arm("the full set passes this check (the build then stops later, on the stand-in tree's missing sources)",
        "was built with patches" not in out, out)
rm = (R / "build/build_rm620.sh").read_text()
arm("build_rm620.sh applies every patch with no fuzz and records it", 'patch -p1 --fuzz=0 --no-backup-if-mismatch < "$f"' in rm and 'basename "$f" >> "$T/.nm-patches"' in rm)
arm("and refuses a work tree it did not patch", "has no record of the patches applied to it" in rm)
fb = (R / "kexts/NVRMFB/fb/nvrm-fb.cpp").read_text()
arm("NVRMFB cannot be compiled against an interface without the colour fields", "static_assert(sizeof(nvrmHasColorFields<" in fb)
print("FAILED" if fails else "PASS"); sys.exit(1 if fails else 0)
