#!/usr/bin/env python3
"""install.sh refuses a /Library or /Library/NullMoth that another account could write through an access-control list.
2.5.3 field logs: one Mac stopped on every run because the check refused ANY entry, deny-only ones included."""
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
src = (ROOT / "package/install.sh").read_text()
start = src.index("require_install_directory() {")
fn = src[start:src.index("\n}\n", start) + 3]
fails = 0


def run(listing):
    # stat and ls are replaced by shell functions; the directory checked is this tools folder, which exists
    script = f"""set -euo pipefail
die() {{ echo "DIE $*"; exit 3; }}
stat() {{ echo "0 755"; }}
ls() {{ printf '%s\\n' {" ".join("'" + l + "'" for l in listing)}; }}
{fn}
require_install_directory "$D"
echo PASSED
"""
    d = ROOT / "tools"
    p = subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, env={"D": str(d), "PATH": "/usr/bin:/bin"})
    return p.stdout + p.stderr


def arm(name, ok, detail=""):
    global fails
    fails += not ok
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail.strip()[:300]})" if detail and not ok else ""))


out = run(["drwxr-xr-x  69 root  wheel  2208 Sep 16 16:53 /Library"])
arm("a directory without an access-control list passes", "PASSED" in out, out)
out = run(["drwxr-xr-x+ 69 root  wheel  2208 Sep 16 16:53 /Library", " 0: group:everyone deny delete"])
arm("deny-only entries pass: they only take access away", "PASSED" in out, out)
out = run(["drwxr-xr-x+ 69 root  wheel  2208 Sep 16 16:53 /Library", " 0: group:everyone deny delete",
           " 1: user:someone allow add_file,add_subdirectory,delete_child"])
arm("an allow entry stops the install", "DIE" in out and "PASSED" not in out, out)
arm("and the stop names the entry", "user:someone allow add_file" in out, out)
out = run(["drwxr-xr-x+ 69 root  wheel  2208 Sep 16 16:53 /Library", " 0: group:staff inherited allow list,search"])
arm("an inherited allow entry stops it too", "DIE" in out, out)
print(f"{fails} failed")
sys.exit(1 if fails else 0)
