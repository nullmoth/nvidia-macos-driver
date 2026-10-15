#!/usr/bin/env python3
"""NVRMUserClient refuses an unknown user-client type instead of treating it as a device client (R1).

The type comes from user space. Only two are valid: 0 = ctl (escalated RM state), 1 = device. The old code did
fIsCtl = (type == 0) with no bound, so any non-zero value -- including a bogus 0x4841 -- opened a working device
client. The fix refuses type > 1. The ctl client stays open to unprivileged callers on purpose (the Vulkan userland
opens it as the logged-in user); the operations behind it are gated elsewhere (real-privilege check, root on first
init). This models the decision table out of the kext source and checks the guard is present, with the pre-fix source
as the negative control."""
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / "kexts/NVRM/NVRM.cpp").read_text()
fails = 0


def check(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    fails += not ok


def decide(body, type_value):
    """Return 'ctl', 'device', or 'refused' for a given type, by reading the init body's guard + fIsCtl assignment."""
    refused = re.search(r"if \(type > (\d+)\) return false;", body)
    if refused and type_value > int(refused.group(1)):
        return "refused"
    m = re.search(r"fIsCtl = \(type == (\d+)\);", body)
    assert m, "fIsCtl assignment not found"
    return "ctl" if type_value == int(m.group(1)) else "device"


i = src.index("::initWithTask(")
body = src[i:src.index("\n}\n", i)]

check("type 0 opens the ctl client", decide(body, 0) == "ctl")
check("type 1 opens the device client", decide(body, 1) == "device")
check("type 2 is refused", decide(body, 2) == "refused")
check("a bogus type 0x4841 is refused", decide(body, 0x4841) == "refused")

old = subprocess.run(["git", "-C", str(root), "show", "v1.15.0:kexts/NVRM/NVRM.cpp"], capture_output=True, text=True)
if old.returncode:
    sys.exit("could not read NVRM.cpp at v1.15.0 - negative control is UNKNOWN, not clean")
oi = old.stdout.index("::initWithTask(")
obody = old.stdout[oi:old.stdout.index("\n}\n", oi)]
check("negative control: the v1.15.0 client accepts a bogus type as a device client",
      decide(obody, 0x4841) == "device")
sys.exit(1 if fails else 0)
