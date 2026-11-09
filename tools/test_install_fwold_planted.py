#!/usr/bin/env python3
"""A rollback must not write the firmware through a /Users/Shared/nvfw that another account planted.

/Users/Shared is writable by every account. The rollback (in install.sh and in the boot-time revert it writes) used
`mkdir -p "$FWOLD" && ditto ...`: mkdir -p accepts an existing folder or a symlink to one, so root copied the firmware
wherever the link pointed, or into a folder that account owns and can change afterwards.
The arms run the exact rollback lines from package/install.sh with FWOLD pointing at a planted symlink and at a
planted folder, and once with nothing there (the copy must still happen).
Usage: test_install_fwold_planted.py [install.sh]"""
import pathlib
import re
import subprocess
import sys
import tempfile

SRC = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1] / "package/install.sh"
text = SRC.read_text()
FAILS = []


def arm(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        FAILS.append(name)


lines = [l.strip() for l in text.splitlines()
         if not l.strip().startswith("#") and "FWOLD" in l and "BK" in l and "nvfw" in l and "ditto" in l
         and "backup_copy" not in l]
arm("install.sh has the two rollback lines that restore the old firmware location", len(lines) == 2, str(len(lines)))


def run(line, planted):
    """planted: 'link', 'dir' or None. Returns (target_written, fwold_written)."""
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d)
        bk = d / "bk"; (bk / "nvfw/nvidia").mkdir(parents=True); (bk / "nvfw/nvidia/gsp.bin").write_text("fw")
        target = d / "elsewhere"; target.mkdir()
        fwold = d / "shared-nvfw"
        if planted == "link":
            fwold.symlink_to(target)
        elif planted == "dir":
            fwold.mkdir()
        body = line.replace("\\$BK", "$BK")
        script = f'BK={bk}; FWOLD={fwold}; failed=0\n{body}\n'
        subprocess.run(["/bin/bash", "-c", script], capture_output=True)
        return (target / "nvidia/gsp.bin").exists(), (fwold / "nvidia/gsp.bin").exists() and not fwold.is_symlink()


for i, line in enumerate(lines):
    tw, _ = run(line, "link")
    arm(f"rollback line {i + 1}: a planted symlink is not written through", not tw)
    _, fw = run(line, "dir")
    arm(f"rollback line {i + 1}: a planted folder is not adopted", not fw)
    _, fw = run(line, None)
    arm(f"rollback line {i + 1}: with nothing there, the old location is still restored", fw)

# the backup taken before an install: the old location counts only when root owns it (not a symlink, uid 0)
bk = [l for l in text.splitlines() if "backup_copy firmware" in l and "FWOLD" in l]
arm("the old firmware location is backed up only when it is root's own folder",
    len(bk) == 1 and '! -L "$FWOLD"' in bk[0] and "$1==0" in bk[0] and "[2367]" in bk[0], bk[0].strip() if bk else "none")

print(f"{len(FAILS)} failed")
sys.exit(1 if FAILS else 0)
