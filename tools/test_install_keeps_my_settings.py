#!/usr/bin/env python3
"""install.sh replaces /Library/GPUBundles/nvmtl wholesale; a user's own settings block in nvrm610.conf must survive
(GH #130: an RTX 3050 needed two knobs and every update dropped them), and only driver knobs may come through."""
import os, pathlib, subprocess, sys, tempfile

SRC = (pathlib.Path(__file__).resolve().parents[1] / "package/install.sh").read_text()
start = SRC.index("MYCONF_START=")
end = SRC.index("if [ -d \"$EXT/NMXGPU.kext\" ]", start)
block = SRC[start:end].replace(
    'for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl; do rm -rf "$GB/$b" && ditto "$HERE/Library/GPUBundles/$b" "$GB/$b" || die "copy $b (restore from $BK)"; done',
    'rm -rf "$GB/nvmtl" && cp -R "$HERE/Library/GPUBundles/nvmtl" "$GB/nvmtl"')
assert "cp -R" in block, "the copy line moved: update this test's anchor"
fails = []


def arm(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail and not ok else ""))
    if not ok:
        fails.append(name)


def run(old, new):
    with tempfile.TemporaryDirectory() as t:
        gb, here = os.path.join(t, "gb"), os.path.join(t, "here")
        os.makedirs(os.path.join(gb, "nvmtl")); os.makedirs(os.path.join(here, "Library/GPUBundles/nvmtl"))
        if old is not None:
            open(os.path.join(gb, "nvmtl/nvrm610.conf"), "w").write(old)
        open(os.path.join(here, "Library/GPUBundles/nvmtl/nvrm610.conf"), "w").write(new)
        script = f'GB="{gb}"; HERE="{here}"; T="{t}"; ok() {{ echo "ok $*"; }}; die() {{ echo "DIE $*"; exit 1; }}\n' + block
        r = subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True)
        return r.returncode, open(os.path.join(gb, "nvmtl/nvrm610.conf")).read(), r.stdout + r.stderr


M0, M1 = "# --- my settings, kept on update ---", "# --- end my settings ---"
rc, out, log = run(f"OLD=1\n{M0}\nNVMTL_NO_SURFACE_PAGEOFF=1\nrm -rf /\nNVK_CMDPOOL_UNCACHED=1\n{M1}\n", "NVK_NEW=2\n")
arm("the user's knobs are kept across an update", rc == 0 and "NVMTL_NO_SURFACE_PAGEOFF=1" in out and "NVK_CMDPOOL_UNCACHED=1" in out, log)
arm("the package's own settings are the new ones", "NVK_NEW=2" in out and "OLD=1" not in out)
arm("anything that is not a driver knob is dropped", "rm -rf" not in out, out)
rc, out, log = run("OLD=1\n", "NVK_NEW=2\n")
arm("no block: the file is exactly the package's", rc == 0 and out == "NVK_NEW=2\n", repr(out))
rc, out, log = run(None, "NVK_NEW=2\n")
arm("a first install (no old file) works", rc == 0 and out == "NVK_NEW=2\n", log)
print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
