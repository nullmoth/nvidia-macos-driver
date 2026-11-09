#!/usr/bin/env python3
"""Does a laptop's DSDT carry a keyboard-backlight interface NMKbdBacklight uses (ASUS WMI, Lenovo IdeaPad KBLC or HALS/SALS, ThinkPad MLCG/MLCS)? usage: check_kbl_dsdt.py <DSDT.aml>...
Prints one line per table and exits 1 if a table named on the command line lacks it. Reads the AML bytes only: the method
names WMNB, DSTS and DEVS, and the device id 0x00050021 as a 32-bit constant (opcode 0x0C), which the method compares
against. A table from another vendor is the control: it must not match."""
import sys

def kinds(data):
    """The keyboard backlight interfaces NMKbdBacklight can use that a DSDT carries, by the method names and ids it checks."""
    if data[:4] != b"DSDT":
        return None
    out = []
    if all(n in data for n in (b"WMNB", b"DSTS", b"DEVS")) and b"\x0c\x21\x00\x05\x00" in data:
        out.append("asus-wmi")
    if b"VPC2004" in data and b"KBLC" in data:
        out.append("lenovo-ideapad-kblc")
    if b"VPC2004" in data and b"HALS" in data and b"SALS" in data:
        out.append("lenovo-ideapad-plain")
    if b"MLCG" in data and b"MLCS" in data and (b"LEN0068" in data or b"IBM0068" in data or b"LEN0268" in data):
        out.append("lenovo-thinkpad")
    return out


def check(data):
    k = kinds(data)
    if k is None:
        return "not a DSDT"
    return "interface present: " + ", ".join(k) if k else "absent"


if __name__ == "__main__":
    bad = 0
    for p in sys.argv[1:]:
        r = check(open(p, "rb").read())
        print(f"{p}: {r}")
        bad += 0 if r.startswith("interface present") else 1
    sys.exit(1 if bad else 0)
