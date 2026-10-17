#!/usr/bin/env python3
"""NVKMS copies never take a kernel pointer from a caller, the NVKMS session is root-only, and no process can fill the
client table (security report #214, findings 2 and 4).

Finding 2: nvkms_copyin/nvkms_copyout treated an address with the top bit set as a kernel pointer and copied it with
memcpy, so a value placed in an NVKMS request became an arbitrary kernel read or write. They now refuse such an
address and only ever copy to or from the calling process. The user-facing NVKMS entry (sKms, sKmsEvents) opened a
kernel-space NVKMS client, whose embedded pointers NVKMS trusts. It was made root-only, and since nothing the driver
ships called it, selectors 4 and 5 are now retired: no process, root included, can open that session.

Finding 4: one global client table, no per-process limit. A process now holds at most kNvClientsPerPid clients and the
last kNvClientsReserved slots are kept for system accounts, so WindowServer can always open one.

This reads the decisions out of the kext source; the negative control is the 1.15.0 source, which must fail each check.
"""
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0


def check(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    fails += not ok


def body(src, sig):
    i = src.index(sig)
    return src[i:src.index("\n}\n", i)]


def copies_safe(kms):
    """Neither copy function memcpys, and both refuse a top-bit address before copying."""
    for sig in ("int nvkms_copyin(", "int nvkms_copyout("):
        b = body(kms, sig)
        if "memcpy" in b or not re.search(r"if \(uaddr >> 63\) return -14;", b):
            return False
    return True


def selectors(nvrm):
    """The user client's dispatch table, in selector order."""
    b = body(nvrm, "IOReturn NVRMUserClient::externalMethod(")
    return re.findall(r"^\s*\{ (\w+),", b[b.index("tbl[] = {"):b.index("};")], re.M)


def kms_unreachable(nvrm):
    """Selectors 4 and 5 are retired stubs, sWaitEvent stays 6 (NVK calls it by number), nothing opens a session."""
    t = selectors(nvrm)
    return (t[4:7] == ["sRetired", "sRetired", "sWaitEvent"] and "nvkms_xnu_open" not in nvrm
            and "return kIOReturnUnsupported;" in body(nvrm, "IOReturn NVRMUserClient::sRetired("))


def quota(nvrm):
    """initWithTask counts this pid's clients and refuses past both limits before it takes a slot."""
    b = body(nvrm, "::initWithTask(")
    m = re.search(r"kNvClientsPerPid = (\d+), kNvClientsReserved = (\d+)", b)
    if not m:
        return None
    take = b.find("gNvClients[slot] = this")
    refuse = b.find("!overPid && !overShared")
    return (int(m.group(1)), int(m.group(2))) if 0 <= refuse < take else None


kms = (root / "kexts/NVRM/os-nvkms-xnu.cpp").read_text()
nvrm = (root / "kexts/NVRM/NVRM.cpp").read_text()

check("nvkms_copyin/copyout refuse a kernel-range address and never memcpy", copies_safe(kms))
check("selectors 4 and 5 are retired; nothing in user space can open a kernel-space NVKMS session", kms_unreachable(nvrm),
      str(selectors(nvrm)))
check("NVKMS has no entry left that opens a session for a user client", "nvkms_xnu_open" not in kms)
q = quota(nvrm)
check("a process is limited and system slots are reserved, before a slot is taken", q is not None, str(q))
if q:
    # the busiest process measured on a live desktop held 154 clients (Maps); the limit must leave wide room for it
    check("the per-process limit is far above the busiest measured process (154)", q[0] >= 154 * 10, f"{q[0]}")
    check("the reserve leaves most of the table to ordinary apps", q[1] <= 32768 // 4, f"{q[1]}")


def old(path):
    r = subprocess.run(["git", "-C", str(root), "show", f"v1.15.0:{path}"], capture_output=True, text=True)
    if r.returncode:
        sys.exit(f"could not read {path} at v1.15.0 - negative control is UNKNOWN, not clean")
    return r.stdout


okms, onvrm = old("kexts/NVRM/os-nvkms-xnu.cpp"), old("kexts/NVRM/NVRM.cpp")
check("negative control: 1.15.0 memcpys a top-bit address in nvkms_copyin", not copies_safe(okms))
check("negative control: 1.15.0 dispatches selector 4 to sKms", not kms_unreachable(onvrm) and selectors(onvrm)[4] == "sKms")
check("negative control: 1.15.0 has no client quota", quota(onvrm) is None)
sys.exit(1 if fails else 0)
