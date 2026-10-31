#!/usr/bin/env python3
"""The set-mode reason line runs on a refused mode set (kexts/NVRM/patches/610-setmode-reason.patch).

NVKMS's SET_MODE ioctl returns false whenever the reply status is not success, and KmsSetMode jumps to `done` on a
false ioctl. The first version of this patch logged the reason after that jump, so it never ran: no upload ever held
"[kapi] set mode refused", while a GTX 1660 on 2.6.5 logged "NVKMS ioctl 9 refused" and "commit rejected ... flipResult
1" with no reason. The line must come before `if (!status)`. Pass a patched nvkms-kapi.c as argv[1] to check a tree."""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
patch = (ROOT / "kexts/NVRM/patches/610-setmode-reason.patch").read_text()
fails = 0


def arm(name, ok, detail=""):
    global fails
    fails += not ok
    print(("ok   " if ok else "FAIL ") + name + ("" if ok else f" ({detail})"))


def check_source(text, label):
    i_warn = text.find('"[kapi] set mode refused')
    arm(f"{label}: the reason line exists", i_warn >= 0)
    i_status = text.find("    if (!status) {\n        nvKmsKapiLogDeviceDebug(", max(i_warn, 0))
    arm(f"{label}: the reason line runs just before the ioctl result jumps to done",
        i_warn >= 0 and 0 < i_status - i_warn < 700, f"{i_warn} {i_status}")
    tail = text[i_status:i_status + 300]
    arm(f"{label}: that check is the SET_MODE ioctl's", "NVKMS_IOCTL_SET_MODE ioctl failed" in tail or label == "patch hunk", tail[:80])
    guard = text.rfind("if (params->reply.status != NVKMS_SET_MODE_STATUS_SUCCESS)", 0, i_warn)
    arm(f"{label}: and only when the reply status is not success", 0 <= guard and i_warn - guard < 1200, f"{guard} {i_warn}")


# Rebuild the post-patch hunk text from the patch's context and added lines.
after = "\n".join(l[1:] for l in patch.splitlines() if l[:1] in (" ", "+") and not l.startswith("+++")) + "\n"
check_source(after, "patch hunk")
arm("the line says whether the ioctl itself returned false", 'ioctl %s' in patch and 'status ? "ok" : "false"' in patch)
arm("negative control: no reason line left after the ioctl-failed jump",
    not re.search(r'NVKMS_IOCTL_SET_MODE ioctl failed"\);\n\+?\s*goto done;\n\+?\s*\}\n[\s\S]{0,400}\+\s*nvEvoLog', patch))
if len(sys.argv) > 1:
    check_source(pathlib.Path(sys.argv[1]).read_text(), "tree")
print(f"{fails} failed")
sys.exit(1 if fails else 0)
