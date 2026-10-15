#!/usr/bin/env python3
"""The PV-SENTINEL verifies the running kernel before it touches any baked address, and refuses on a mismatch (R5).

os-xnu2.cpp carries fixed kernel addresses (kKdk*) for one BootKC build. R5's requirement is: verify each address
against the running kernel before use and refuse to load on a mismatch, so a wrong build fails cleanly instead of
reading garbage. nv_pv_arm() must (1) slide the addresses by the live &kprintf, (2) compare 16 bytes of
_pmap_verify_free text against the known build and return a REFUSED string on any difference, and (3) only AFTER those
guards assign nv_pv_head/attr/last/kpmap. This checks that ordering in the source; the negative control is the same
body with the verify-return deleted, which would reach the assignment unguarded."""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / "kexts/NVRM/os-xnu2.cpp").read_text()
fails = 0


def check(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    fails += not ok


i = src.index("nv_pv_arm(void)")
body = src[i:src.index("\n}\n", i)]


def guarded(b):
    """True if a REFUSED return on the text-byte mismatch precedes the first baked-pointer assignment."""
    verify = re.search(r"if \(vf\[i\] != kKdkVerifyBytes\[i\]\) return \"REFUSED", b)
    assign = b.find("nv_pv_head = *")
    return verify is not None and assign != -1 and verify.start() < assign


check("the slide is computed from the live &kprintf", "&kprintf - kKdkKprintf" in body)
check("16 text bytes are verified against the known build", "kKdkVerifyBytes[i]" in body)
check("pmap_initialized is checked", "pmap_initialized != 1" in body or "kKdkPmapInit + slide) != 1" in body)
check("table pointers are plausibility-checked", "table pointers implausible" in body)
check("the verify-refuse precedes using any baked address", guarded(body))

# negative control: delete the verify-return line -> the baked pointers would be used unguarded
stripped = re.sub(r"\s*for \(int i = 0; i < 16; i\+\+\) if \(vf\[i\] != kKdkVerifyBytes\[i\]\) return \"REFUSED[^\n]*\n", "\n", body)
check("negative control: without the verify-return the arm is no longer guarded", not guarded(stripped))
sys.exit(1 if fails else 0)
