#!/usr/bin/env python3
"""Every SHA256SUMS check in the driver installer is strict: shasum -c without --strict skips a malformed line and
still exits 0, so a list with a bad line verified nothing for that file."""
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    fails += not ok


for rel in ("package/install.sh",):  # 1401 checks its own setup script (mac/tools)
    src = (ROOT / rel).read_text()
    checks = re.findall(r"shasum -a 256[^\n|;)]*-c\b[^\n]*", src)
    lax = [c for c in checks if "--strict" not in c]
    arm(f"{rel}: every shasum -c is --strict ({len(checks)} found)", checks and not lax, "; ".join(lax))

with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    (d / "a").write_text("hi\n")
    good = subprocess.run(["shasum", "-a", "256", "a"], cwd=d, capture_output=True, text=True).stdout
    (d / "S").write_text(good + "0000  a\n")
    lax = subprocess.run(["shasum", "-a", "256", "-c", "S", "--quiet"], cwd=d, capture_output=True).returncode
    strict = subprocess.run(["shasum", "-a", "256", "--strict", "-c", "S", "--quiet"], cwd=d, capture_output=True).returncode
    arm("a malformed line passes plain shasum -c (the bug this guards against)", lax == 0, f"rc {lax}")
    arm("a malformed line fails shasum --strict -c", strict != 0, f"rc {strict}")
    (d / "S").write_text(good)
    ok = subprocess.run(["shasum", "-a", "256", "--strict", "-c", "S", "--quiet"], cwd=d, capture_output=True).returncode
    arm("a correct list passes shasum --strict -c", ok == 0, f"rc {ok}")

print(f"{fails} failed")
sys.exit(1 if fails else 0)
