#!/usr/bin/env python3
"""NMSMN's access policy (kexts/NMSMN/nmsmn_policy.h) run on the host, and the structure of the kext around it.
Policy arms (compiled C): every mailbox register passes and nothing else does; a write outside the mailboxes is refused; a
response register can only be cleared; the power table becomes readable only after the platform SMU answered a table-address
message with success, at the address in ITS answer (32-bit message 0xB, 64-bit message 0x66); a request the SMU rejected,
a table message sent to the other SMU, or arguments the client wrote itself never produce an address; a new table-address request withdraws the old address while other messages keep it; reads are bounded to 64 KiB without wrap-around.
Kext arms (source): off unless nmsmn=1; Zen CPU and host bridge only; the caller must be root, hold the entitlement and be
named ryzenadj; no call takes a physical address; table copies are bounded twice and under the lock; nothing returned is a
kernel pointer. Read at a git revision (default: working tree); a revision before the policy existed must FAIL.
  usage: test_nmsmn_policy.py [rev]
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


hdr, cpp = show("kexts/NMSMN/nmsmn_policy.h"), show("kexts/NMSMN/nmsmn.cpp")
bad = 0


def check(n, ok):
    global bad
    bad += not ok
    print(("ok   " if ok else "FAIL ") + n)


T = r'''
#include <stdio.h>
#include "nmsmn_policy.h"
#define P(k, v) printf(k "=%d\n", (int)(v))
int main(void)
{
    struct nmsmn_state s; nmsmn_state_init(&s);
    int all = 1;
    for (unsigned i = 0; i < sizeof kNmsmnMailboxes / sizeof kNmsmnMailboxes[0]; i++) {
        const struct nmsmn_mailbox *m = &kNmsmnMailboxes[i];
        all &= nmsmn_allowed(m->msg) && nmsmn_allowed(m->rsp);
        for (int a = 0; a < NMSMN_ARGS; a++) all &= nmsmn_allowed(m->arg + 4 * a);
    }
    P("all_allowed", all);
    P("write_outside", nmsmn_check_write(&s, 0x3B10000, 1));
    P("write_unaligned", nmsmn_check_write(&s, 0x3B10529, 1));
    P("write_zero_addr", nmsmn_check_write(&s, 0, 0));
    P("rsp_set_to_1", nmsmn_check_write(&s, kNmsmnMailboxes[4].rsp, 1));
    P("rsp_clear", nmsmn_check_write(&s, kNmsmnMailboxes[4].rsp, 0));
    int open = 0; for (uint32_t a = 0x3B10000; a < 0x3B11000; a += 4) open += nmsmn_allowed(a);
    P("open_in_page_lt60", open < 60);

    /* 64-bit table address from the PSMU (set 4), answered OK */
    const struct nmsmn_mailbox *m = &kNmsmnMailboxes[4];
    nmsmn_state_init(&s);
    P("before_table_ok", nmsmn_table_range_ok(&s, 0, 8));
    nmsmn_check_write(&s, m->arg, 0xdead); nmsmn_check_write(&s, m->arg + 4, 0xbeef);      /* the client's own values */
    nmsmn_check_write(&s, m->msg, 0x66);
    nmsmn_note_read(&s, m->arg, 0xdead); nmsmn_note_read(&s, m->arg + 4, 0xbeef);          /* read back before any OK */
    P("forged_args_valid", s.table_valid);
    nmsmn_note_read(&s, m->rsp, 0xFE);                                                   /* the SMU rejected it */
    nmsmn_note_read(&s, m->arg, 0x12345000); nmsmn_note_read(&s, m->arg + 4, 0x1);
    P("rejected_valid", s.table_valid);
    nmsmn_check_write(&s, m->msg, 0x66);
    nmsmn_note_read(&s, m->rsp, 1);
    nmsmn_note_read(&s, m->arg, 0x12345000); nmsmn_note_read(&s, m->arg + 4, 0x1);
    printf("wide_addr=%llx valid=%d\n", (unsigned long long)s.table, s.table_valid);
    P("range_ok", nmsmn_table_range_ok(&s, 0, 4096));
    P("range_top", nmsmn_table_range_ok(&s, NMSMN_PM_MAX - 8, 8));
    P("range_past", nmsmn_table_range_ok(&s, NMSMN_PM_MAX - 8, 9));
    P("range_wrap", nmsmn_table_range_ok(&s, 0xFFFFFFF0u, 0x20));
    P("range_len0", nmsmn_table_range_ok(&s, 0, 0));
    P("range_big", nmsmn_table_range_ok(&s, 0, NMSMN_PM_MAX + 1));
    /* other messages (the table refresh among them) keep the address; a new table-address request withdraws it until answered */
    nmsmn_check_write(&s, m->msg, 0x3D);
    P("after_other_msg_valid", s.table_valid);
    nmsmn_check_write(&s, m->msg, 0x66);
    P("after_new_request_valid", s.table_valid);

    /* 32-bit answer */
    nmsmn_state_init(&s);
    nmsmn_check_write(&s, m->msg, 0xB); nmsmn_note_read(&s, m->rsp, 1); nmsmn_note_read(&s, m->arg, 0x7f000000);
    printf("narrow_addr=%llx valid=%d\n", (unsigned long long)s.table, s.table_valid);
    /* the table message on an MP1 mailbox does nothing */
    nmsmn_state_init(&s);
    const struct nmsmn_mailbox *mp1 = &kNmsmnMailboxes[0];
    nmsmn_check_write(&s, mp1->msg, 0x66); nmsmn_note_read(&s, mp1->rsp, 1); nmsmn_note_read(&s, mp1->arg, 0x1000); nmsmn_note_read(&s, mp1->arg + 4, 0);
    P("mp1_valid", s.table_valid);
    /* a zero or out-of-range answer is not an address */
    nmsmn_state_init(&s);
    nmsmn_check_write(&s, m->msg, 0x66); nmsmn_note_read(&s, m->rsp, 1); nmsmn_note_read(&s, m->arg, 0); nmsmn_note_read(&s, m->arg + 4, 0);
    P("zero_valid", s.table_valid);
    nmsmn_state_init(&s);
    nmsmn_check_write(&s, m->msg, 0x66); nmsmn_note_read(&s, m->rsp, 1); nmsmn_note_read(&s, m->arg, 0x1000); nmsmn_note_read(&s, m->arg + 4, 0xFFFF0000);
    P("huge_valid", s.table_valid);
    return 0;
}
'''
g = {}
if not hdr:
    check("the policy header exists", False)
else:
    with tempfile.TemporaryDirectory() as td:
        d = pathlib.Path(td)
        (d / "nmsmn_policy.h").write_text(hdr)
        (d / "t.c").write_text(T)
        r = subprocess.run(["cc", "-w", "-o", str(d / "t"), str(d / "t.c")], capture_output=True, text=True)
        if r.returncode:
            print("FAIL could not compile:\n" + r.stderr[:600]); sys.exit(1)
        for line in subprocess.run([str(d / "t")], capture_output=True, text=True).stdout.splitlines():
            for kv in line.split():
                k, v = kv.split("=")
                g[k] = v
    check("every mailbox register of every SMU set is allowed, and little else near them", g["all_allowed"] == "1" and g["open_in_page_lt60"] == "1")
    check("a write outside the mailboxes (or unaligned, or to address 0) is refused",
          g["write_outside"] == "-1" and g["write_unaligned"] == "-1" and g["write_zero_addr"] == "-1")
    check("a response register can be cleared and not set", g["rsp_set_to_1"] == "-1" and g["rsp_clear"] == "0")
    check("no table can be read before the SMU has answered", g["before_table_ok"] == "0")
    check("arguments the client wrote itself, read back before the SMU said OK, never become an address", g["forged_args_valid"] == "0")
    check("a request the SMU rejected gives no address", g["rejected_valid"] == "0")
    check("the 64-bit answer of the platform SMU is the address, and only it", g["wide_addr"] == "112345000")
    check("table reads stay inside 64 KiB, with no wrap-around and no empty read",
          g["range_ok"] == "1" and g["range_top"] == "1" and g["range_past"] == "0" and g["range_wrap"] == "0"
          and g["range_len0"] == "0" and g["range_big"] == "0")
    check("other messages keep the address; a new table-address request withdraws it until the SMU answers",
          g["after_other_msg_valid"] == "1" and g["after_new_request_valid"] == "0")
    check("the 32-bit answer works", g["narrow_addr"] == "7f000000")
    check("the table message on an MP1 mailbox gives no address", g["mp1_valid"] == "0")
    check("a zero or beyond-48-bit answer is not an address", g["zero_valid"] == "0" and g["huge_valid"] == "0")
check("the kext attaches only with nmsmn=1, to the AMD host bridge at 0:0.0 of a Zen CPU",
      'PE_parse_boot_argn("nmsmn"' in cpp and "on != 1" in cpp and "0x1022" in cpp and "zenCpu()" in cpp
      and "getFunctionNumber() != 0" in cpp)
check("a client must be root, carry the entitlement and be named ryzenadj",
      "kIOClientPrivilegeAdministrator" in cpp and 'copyClientEntitlement(task, "com.nullmoth.ryzenadj")' in cpp
      and 'strncmp(name, "ryzenadj"' in cpp and "proc_selfname" in cpp)
check("no call takes a physical address and the table descriptor is built only from the remembered one",
      "scalarInput[0]" in cpp and "withPhysicalAddress((IOPhysicalAddress)s->table" in cpp
      and cpp.count("withPhysicalAddress") == 1 and "kNMSMNCount" in cpp)
check("table copies are bounded by the chunk size and the client's buffer, and run under the lock",
      "len > NMSMN_CHUNK" in cpp and "len > args->structureOutputSize" in cpp and cpp.index("IOLockLock(lock);\n    IOReturn r = kIOReturnNotPermitted;\n    if (nmsmn_table_range_ok") > 0)
check("the only values returned are register contents and the byte count (no pointers, no addresses)",
      "scalarOutput[0] = v;" in cpp and "structureOutputSize = len" in cpp and "s->table;" not in cpp.replace("(IOPhysicalAddress)s->table", ""))
print("PASS" if not bad else f"{bad} FAILED")
sys.exit(1 if bad else 0)
