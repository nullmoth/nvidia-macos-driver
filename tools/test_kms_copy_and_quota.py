#!/usr/bin/env python3
"""NVKMS copies never take a kernel pointer from a caller, the NVKMS session is root-only, and no process can fill the
client table (security report #214, findings 2 and 4).

Finding 2: nvkms_copyin/nvkms_copyout treated an address with the top bit set as a kernel pointer and copied it with
memcpy, so a value placed in an NVKMS request became an arbitrary kernel read or write. They now refuse such an
address and only ever copy to or from the calling process. The user-facing NVKMS entry (sKms, sKmsEvents) opens a
kernel-space NVKMS client, whose embedded pointers NVKMS trusts, so it is root-only.

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


def kms_root_only(nvrm):
    """sKms and sKmsEvents both refuse a non-root caller before doing anything else."""
    for sig in ("IOReturn NVRMUserClient::sKms(", "IOReturn NVRMUserClient::sKmsEvents("):
        b = body(nvrm, sig)
        gate = b.find("kauth_getuid() != 0) return kIOReturnNotPrivileged")
        work = max(b.find("nvkms_xnu_open"), b.find("nvkms_xnu_events_available"))
        if gate < 0 or (work >= 0 and gate > work):
            return False
    return True


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
check("sKms and sKmsEvents are root-only, gated before the NVKMS session is touched", kms_root_only(nvrm))
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
check("negative control: 1.15.0 lets any caller reach sKms", not kms_root_only(onvrm))
check("negative control: 1.15.0 has no client quota", quota(onvrm) is None)
sys.exit(1 if fails else 0)
