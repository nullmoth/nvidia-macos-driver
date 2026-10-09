#!/usr/bin/env python3
"""install.sh checks SHA256SUMS and then copies the kexts minutes later, after the test kernel collection build. The
extracted package is usually writable by the logged-in user, so the check and the copy must use a root-owned private
copy: a file changed in the original after the check must not reach /Library/Extensions, and a link that leads out of
the package (shasum follows it, so it passes the check) must be refused. Links inside the package (nvmtl's
libvulkan.dylib) still install."""
import hashlib
import os
import pathlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC = (ROOT / "package/install.sh").read_text()
start = SRC.index('step "2. package integrity"')
end = SRC.index("for target in 15 26; do", start)
BLOCK = SRC[start:end]
assert "STAGE=$(mktemp -d /var/tmp/" in BLOCK, "the private copy moved: update this test's anchor"
fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail and not ok else ""))
    fails += not ok


def package(t, links=()):
    here = t / "pkgroot"
    kext = here / "Library/Extensions/NVRM.kext/Contents/MacOS/NVRM"
    kext.parent.mkdir(parents=True); kext.write_bytes(b"checked-NVRM")
    lib = here / "Library/GPUBundles/nvmtl/libvulkan.1.dylib"
    lib.parent.mkdir(parents=True); lib.write_bytes(b"vulkan")
    for name, target in links:
        (lib.parent / name).symlink_to(target)
    files = sorted(p for p in here.rglob("*") if p.is_file() and not p.is_symlink())
    (here / "SHA256SUMS").write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(here)}\n" for p in files))
    return here


def run(t, here, after="", payload=False):
    """Run step 2 as install.sh would, then `after` (the attacker's move and the later copy) in the same shell."""
    (t / "tmp").mkdir(exist_ok=True)
    block = BLOCK.replace("/var/tmp/", f"{t}/tmp/")
    script = ('step() { :; }; ok() { :; }; die() { echo "DIE $*"; exit 1; }; chown() { :; }\n'
              f'HERE="{here}"\n' + block + f"ORIG='{here}'\n" + after)
    where = t / "elsewhere" if payload else here
    where.mkdir(exist_ok=True)
    (where / "install.sh").write_text(script)
    r = subprocess.run(["/bin/bash", str(where / "install.sh")], capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr


with tempfile.TemporaryDirectory() as d:
    t = pathlib.Path(d); here = package(t, [("libvulkan.dylib", "libvulkan.1.dylib")])
    rc, out = run(t, here, after='''
printf swapped > "$ORIG/Library/Extensions/NVRM.kext/Contents/MacOS/NVRM"
echo "COPIED=$(cat "$HERE/Library/Extensions/NVRM.kext/Contents/MacOS/NVRM")"
echo "HERE=$HERE"; echo "SELF=$SELF"
find "$HERE" -perm -g+w -o -perm -o+w | sed 's/^/WRITABLE=/'
''')
    arm("a clean package passes the check", rc == 0, out)
    arm("a kext swapped in the original after the check is not what gets copied", "COPIED=checked-NVRM" in out, out)
    arm("the install reads from the private copy, not the extracted folder", f"HERE={t}/tmp/nullmoth-package." in out, out)
    arm("the runtime checker beside install.sh is taken from the private copy too", f"SELF={t}/tmp/nullmoth-package." in out, out)
    arm("nothing in the private copy is writable by group or others", "WRITABLE=" not in out, out)
    arm("a link that stays inside the package (libvulkan.dylib) is accepted", "DIE" not in out, out)

with tempfile.TemporaryDirectory() as d:
    t = pathlib.Path(d); here = package(t)
    (here / "Library/Extensions/NVRM.kext/Contents/MacOS/NVRM").write_bytes(b"changed-before")
    rc, out = run(t, here)
    arm("a file changed before the check is refused", rc != 0 and "SHA256SUMS mismatch" in out, out)

for label, target in (("an absolute link", "/etc/hosts"), ("a ../ link", "../../../../outside")):
    with tempfile.TemporaryDirectory() as d:
        t = pathlib.Path(d); (t / "outside").write_bytes(b"vulkan")
        here = package(t, [("libvulkan.dylib", target)])
        rc, out = run(t, here)
        arm(f"{label} out of the package is refused", rc != 0 and "link that leaves the package" in out, out)

with tempfile.TemporaryDirectory() as d:
    t = pathlib.Path(d); here = package(t)
    rc, out = run(t, here, after='echo "SELF=$SELF"\n', payload=True)
    arm("--payload: the trusted installer's own folder is kept for the runtime checker",
        rc == 0 and f"SELF={t}/elsewhere" in out, out)

print("FAILED" if fails else "PASS")
sys.exit(1 if fails else 0)
