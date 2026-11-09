#!/usr/bin/env python3
"""NMSMN's register whitelist (kexts/NMSMN/nmsmn_allow.h), run on the host: only the SMU mailbox registers pass.
Arms: every message, response and argument register of every mailbox is allowed; the byte after an argument block and
unaligned addresses are refused; ordinary SMN space (the SoC's other registers) is refused; the kext opts in by boot
argument, needs the administrator privilege and has no physical-memory call. Read at a git revision (default: working tree);
a revision before the kext existed must FAIL.
  usage: test_nmsmn_allow.py [rev]
"""
import pathlib, subprocess, sys, tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
rev = sys.argv[1] if len(sys.argv) > 1 else None


def show(path):
    if rev:
        r = subprocess.run(["git", "-C", str(ROOT), "show", f"{rev}:{path}"], capture_output=True, text=True)
        return r.stdout if r.returncode == 0 else ""
    p = ROOT / path
    return p.read_text() if p.exists() else ""


hdr, cpp = show("kexts/NMSMN/nmsmn_allow.h"), show("kexts/NMSMN/nmsmn.cpp")
bad = 0


def check(n, ok):
    global bad
    bad += not ok
    print(("ok   " if ok else "FAIL ") + n)


T = r'''
#include <stdio.h>
#include "nmsmn_allow.h"
int main(void)
{
    int all = 1, n = 0;
    for (unsigned i = 0; i < sizeof kNmsmnMailboxes / sizeof kNmsmnMailboxes[0]; i++) {
        const struct nmsmn_mailbox *m = &kNmsmnMailboxes[i];
        all &= nmsmn_allowed(m->msg) && nmsmn_allowed(m->rsp);
        for (int a = 0; a < NMSMN_ARGS; a++) all &= nmsmn_allowed(m->arg + 4 * a);
        n++;
    }
    printf("mailboxes=%d all_allowed=%d\n", n, all);
    const struct nmsmn_mailbox *m = &kNmsmnMailboxes[0];
    printf("after_args=%d unaligned=%d low=%d high=%d zero=%d\n", nmsmn_allowed(m->arg + 4 * NMSMN_ARGS), nmsmn_allowed(m->msg + 1),
           nmsmn_allowed(0x1000), nmsmn_allowed(0xFFFFFFFC), nmsmn_allowed(0));
    int open = 0;
    for (uint32_t a = 0x3B10000; a < 0x3B11000; a += 4) open += nmsmn_allowed(a);
    printf("open_in_mailbox_page=%d\n", open);
    return 0;
}
'''
if not hdr:
    check("the whitelist header exists", False)
else:
    with tempfile.TemporaryDirectory() as td:
        d = pathlib.Path(td)
        (d / "nmsmn_allow.h").write_text(hdr)
        (d / "t.c").write_text(T)
        r = subprocess.run(["cc", "-w", "-o", str(d / "t"), str(d / "t.c")], capture_output=True, text=True)
        if r.returncode:
            print("FAIL could not compile:\n" + r.stderr[:600]); sys.exit(1)
        g = dict(kv.split("=") for kv in subprocess.run([str(d / "t")], capture_output=True, text=True).stdout.split())
    check("every mailbox register of every SMU set is allowed", g["all_allowed"] == "1" and g["mailboxes"] == "6")
    check("the register after an argument block, unaligned addresses and ordinary SMN space are refused",
          g["after_args"] == "0" and g["unaligned"] == "0" and g["low"] == "0" and g["high"] == "0" and g["zero"] == "0")
    check("inside the 4 KiB around the mailboxes only the mailbox registers are open (under 60 of 1024)", int(g["open_in_mailbox_page"]) < 60)
check("the kext attaches only with the nmsmn=1 boot argument", 'PE_parse_boot_argn("nmsmn"' in cpp and "on != 1" in cpp)
check("a client needs the administrator privilege", "kIOClientPrivilegeAdministrator" in cpp)
check("every access goes through the whitelist and there is no physical-memory call",
      "nmsmn_allowed(addr)" in cpp and "withPhysicalAddress" not in cpp and "IOMemoryDescriptor" not in cpp)
print("PASS" if not bad else f"{bad} FAILED")
sys.exit(1 if bad else 0)
