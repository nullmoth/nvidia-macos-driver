#!/usr/bin/env python3
"""NMSMN's access policy (kexts/NMSMN/nmsmn_policy.h) run on the host, and the structure of the kext around it.
Policy arms (compiled C): every mailbox register passes and nothing else does; a response register can only be cleared; a
client cannot send the table-address message to a platform SMU and has no path that makes a table readable; the kext's own
answer is accepted only inside firmware-reserved memory (managed RAM, unaligned, unmapped and malformed-map cases refused)
with the region's remainder as the window.
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
#include <string.h>
#include "nmsmn_policy.h"
#define P(k, v) printf(k " %d\n", (int)(v))

static unsigned char map[40 * 6];
static void desc(int i, uint32_t type, uint64_t start, uint64_t pages)
{
    unsigned char *d = map + 40 * i;
    memset(d, 0, 40);
    d[0] = type & 255; d[1] = (type >> 8) & 255;
    for (int k = 0; k < 8; k++) { d[8 + k] = (start >> (8 * k)) & 255; d[24 + k] = (pages >> (8 * k)) & 255; }
}

int main(void)
{
    struct nmsmn_state s;
    int all = 1;
    for (unsigned i = 0; i < sizeof kNmsmnMailboxes / sizeof kNmsmnMailboxes[0]; i++) {
        const struct nmsmn_mailbox *m = &kNmsmnMailboxes[i];
        all &= nmsmn_allowed(m->msg) && nmsmn_allowed(m->rsp);
        for (int a = 0; a < NMSMN_ARGS; a++) all &= nmsmn_allowed(m->arg + 4 * a);
    }
    P("all_allowed", all);
    int open = 0; for (uint32_t a = 0x3B10000; a < 0x3B11000; a += 4) open += nmsmn_allowed(a);
    P("open_lt60", open < 60);
    P("write_outside", nmsmn_check_write(0x3B10000, 1));
    P("write_unaligned", nmsmn_check_write(0x3B10529, 1));
    P("write_zero_addr", nmsmn_check_write(0, 0));
    P("rsp_set", nmsmn_check_write(kNmsmnMailboxes[4].rsp, 1));
    P("rsp_clear", nmsmn_check_write(kNmsmnMailboxes[4].rsp, 0));
    P("arg_write", nmsmn_check_write(kNmsmnMailboxes[4].arg, 0x1234));
    /* the client cannot send the table-address message to the platform SMU, whichever of its two mailboxes */
    P("tbl32_psmu1", nmsmn_check_write(kNmsmnMailboxes[4].msg, 0xB));
    P("tbl64_psmu1", nmsmn_check_write(kNmsmnMailboxes[4].msg, 0x66));
    P("tbl64_psmu2", nmsmn_check_write(kNmsmnMailboxes[5].msg, 0x66));
    P("tbl_on_mp1", nmsmn_check_write(kNmsmnMailboxes[0].msg, 0x66));
    P("other_psmu_msg", nmsmn_check_write(kNmsmnMailboxes[4].msg, 0x3D));

    /* a client that does everything it is allowed to cannot make a table valid: there is no client path into the state */
    nmsmn_state_init(&s);
    nmsmn_check_write(kNmsmnMailboxes[4].arg, 0x12345000); nmsmn_check_write(kNmsmnMailboxes[4].arg + 4, 0);
    nmsmn_check_write(kNmsmnMailboxes[4].rsp, 0); nmsmn_check_write(kNmsmnMailboxes[4].msg, 0x3D);
    P("client_sequence_valid", s.table_valid);
    P("range_before", nmsmn_table_range_ok(&s, 0, 8));

    /* acceptance of the SMU's answer against the firmware's memory map */
    desc(0, 7, 0x100000, 0x7f000);                 /* conventional RAM */
    desc(1, 0, 0x80000000ull, 0x40);               /* reserved, 256 KiB */
    desc(2, 10, 0x90000000ull, 0x4);               /* ACPI NVS, 16 KiB */
    desc(3, 9, 0xA0000000ull, 0x100);              /* ACPI reclaim */
    desc(4, 4, 0xB0000000ull, 0x100);              /* boot services data */
    desc(5, 6, 0xC0000000ull, 0x2);                /* runtime data, 8 KiB */
    const uint32_t size = sizeof map, stride = 40;
    P("accept_reserved", nmsmn_table_accept(&s, 0x80000000ull, map, size, stride));
    printf("limit_reserved %u\n", s.limit);
    P("range_in_ok", nmsmn_table_range_ok(&s, 0, 4096));
    P("range_past_limit", nmsmn_table_range_ok(&s, 0x10000 - 8, 16));
    P("accept_nvs", nmsmn_table_accept(&s, 0x90000000ull, map, size, stride));
    printf("limit_nvs %u\n", s.limit);
    P("range_past_nvs", nmsmn_table_range_ok(&s, 0x4000 - 8, 16));
    P("accept_runtime_mid", nmsmn_table_accept(&s, 0xC0001000ull, map, size, stride));
    printf("limit_runtime_mid %u\n", s.limit);
    P("ram_conventional", nmsmn_table_accept(&s, 0x200000ull, map, size, stride));
    P("ram_reclaim", nmsmn_table_accept(&s, 0xA0000000ull, map, size, stride));
    P("ram_bootdata", nmsmn_table_accept(&s, 0xB0001000ull, map, size, stride));
    P("ram_leaves_state_invalid", s.table_valid);
    P("not_in_map", nmsmn_table_accept(&s, 0xD0000000ull, map, size, stride));
    P("unaligned", nmsmn_table_accept(&s, 0x80000010ull, map, size, stride));
    P("zero", nmsmn_table_accept(&s, 0, map, size, stride));
    P("above_48", nmsmn_table_accept(&s, 1ull << 48, map, size, stride));
    P("small_stride", nmsmn_table_accept(&s, 0x80000000ull, map, size, 24));
    P("short_map", nmsmn_table_accept(&s, 0x80000000ull, map, 20, stride));
    P("null_map", nmsmn_table_accept(&s, 0x80000000ull, 0, 0, stride));
    P("after_refusal_range", nmsmn_table_range_ok(&s, 0, 8));

    /* the message by CPU */
    uint32_t msg, arg0; int wide;
    P("raven", nmsmn_table_message(0x17, 17, &msg, &arg0, &wide)); printf("raven_msg %x %u %d\n", msg, arg0, wide);
    P("renoir", nmsmn_table_message(0x17, 96, &msg, &arg0, &wide)); printf("renoir_msg %x %d\n", msg, wide);
    P("phoenix", nmsmn_table_message(0x19, 116, &msg, &arg0, &wide)); printf("phoenix_msg %x %d\n", msg, wide);
    P("strix", nmsmn_table_message(0x1A, 32, &msg, &arg0, &wide));
    P("dragon_range", nmsmn_table_message(0x19, 97, &msg, &arg0, &wide));
    P("vangogh", nmsmn_table_message(0x17, 144, &msg, &arg0, &wide));
    P("zen1_desktop", nmsmn_table_message(0x17, 1, &msg, &arg0, &wide));
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
            parts = line.split()
            g[parts[0]] = parts[1]
    def v(k):
        return g[k]
    check("every mailbox register of every SMU set is allowed, and little else near them", v("all_allowed") == "1" and v("open_lt60") == "1")
    check("a write outside the mailboxes (or unaligned, or to address 0) is refused",
          v("write_outside") == "-1" and v("write_unaligned") == "-1" and v("write_zero_addr") == "-1")
    check("a response register can be cleared and not set; argument registers can be written", v("rsp_set") == "-1" and v("rsp_clear") == "0" and v("arg_write") == "0")
    check("a client cannot send the table-address message (0xB or 0x66) to either platform-SMU mailbox",
          v("tbl32_psmu1") == "-1" and v("tbl64_psmu1") == "-1" and v("tbl64_psmu2") == "-1")
    check("the same numbers are ordinary messages elsewhere (MP1) and other platform-SMU messages still pass",
          v("tbl_on_mp1") == "0" and v("other_psmu_msg") == "0")
    check("a client that writes argument 0 (or anything else it may) never makes a table readable",
          v("client_sequence_valid") == "0" and v("range_before") == "0")
    check("an address in reserved, runtime or ACPI NVS memory is accepted, with the region's remainder (at most 64 KiB) as the window",
          v("accept_reserved") == "0" and v("limit_reserved") == "65536" and v("accept_nvs") == "0" and v("limit_nvs") == "16384"
          and v("accept_runtime_mid") == "0" and v("limit_runtime_mid") == "4096")
    check("reads stay inside that window", v("range_in_ok") == "1" and v("range_past_limit") == "0" and v("range_past_nvs") == "0")
    check("an address inside RAM the kernel may manage (conventional, ACPI reclaim, boot services data) is refused and leaves nothing valid",
          v("ram_conventional") == "-1" and v("ram_reclaim") == "-1" and v("ram_bootdata") == "-1" and v("ram_leaves_state_invalid") == "0")
    check("an address outside the map, unaligned, zero or beyond 48 bits is refused",
          v("not_in_map") == "-1" and v("unaligned") == "-1" and v("zero") == "-1" and v("above_48") == "-1")
    check("a malformed memory map (stride too small, truncated, absent) refuses everything", v("small_stride") == "-1" and v("short_map") == "-1" and v("null_map") == "-1" and v("after_refusal_range") == "0")
    check("the table message comes from the CPU: 0xB with argument 3 on Raven, 0x66 (64-bit) from Renoir on, none on Van Gogh, Dragon Range or unknown models",
          v("raven") == "0" and g["raven_msg"] == "b" and v("renoir") == "0" and v("phoenix") == "0" and v("strix") == "0"
          and v("dragon_range") == "-1" and v("vangogh") == "-1" and v("zen1_desktop") == "-1")
check("the kext attaches only with nmsmn=1, to the AMD host bridge at 0:0.0 of a Zen CPU",
      'PE_parse_boot_argn("nmsmn"' in cpp and "on != 1" in cpp and "0x1022" in cpp and "zenCpu()" in cpp
      and "getFunctionNumber() != 0" in cpp)
check("a client must be root, carry the entitlement and be named ryzenadj",
      "kIOClientPrivilegeAdministrator" in cpp and 'copyClientEntitlement(task, "com.nullmoth.ryzenadj")' in cpp
      and 'strncmp(name, "ryzenadj"' in cpp and "proc_selfname" in cpp)
check("no call takes a physical address or a table address: the table window is built only from the approved answer, the map copy only from the firmware's boot arguments",
      "withPhysicalAddress((IOPhysicalAddress)s->table, s->limit" in cpp and "withPhysicalAddress((IOPhysicalAddress)ba->MemoryMap" in cpp
      and cpp.count("withPhysicalAddress") == 2 and "kNMSMNCount" in cpp)
check("the kext runs the table-address exchange itself, under its lock, with a bound on the wait, and the client supplies nothing to it",
      "IOReturn NMSMN::tableInit" in cpp and "kNMSMNTableInit && args->scalarInputCount == 0" in cpp and "tries < 2000" in cpp
      and "nmsmn_table_accept(s, addr, map, size, stride)" in cpp and "nmsmn_note_read" not in cpp + hdr)
check("the answer is returned to the client only as a window size, never as an address", "scalarOutput[0] = limit;" in cpp and "s->table;" not in cpp.replace("(IOPhysicalAddress)s->table", ""))
osdep = show("tools/ryzenadj-macos/osdep_macos.c")
check("RyzenAdj's table-address message is turned into the kext's own exchange and played back with a placeholder, never the real address",
      "kTableInit" in osdep and "0x1000u" in osdep and "placeholder" in osdep)
check("table copies are bounded by the chunk size and the client's buffer, and run under the lock",
      "len > NMSMN_CHUNK" in cpp and "len > args->structureOutputSize" in cpp and cpp.index("IOLockLock(lock);\n    IOReturn r = kIOReturnNotPermitted;\n    if (nmsmn_table_range_ok") > 0)
check("the only other values returned are register contents and the byte count (no pointers)",
      "scalarOutput[0] = v;" in cpp and "structureOutputSize = len" in cpp)
print("PASS" if not bad else f"{bad} FAILED")
sys.exit(1 if bad else 0)
