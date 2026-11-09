#!/usr/bin/env python3
"""Does a laptop's DSDT carry the ASUS keyboard-backlight WMI interface NMKbdBacklight uses? usage: check_kbl_dsdt.py <DSDT.aml>...
Prints one line per table and exits 1 if a table named on the command line lacks it. Reads the AML bytes only: the method
names WMNB, DSTS and DEVS, and the device id 0x00050021 as a 32-bit constant (opcode 0x0C), which the method compares
against. A table from another vendor is the control: it must not match."""
import sys

def check(data):
    if data[:4] != b"DSDT":
        return "not a DSDT"
    names = all(n in data for n in (b"WMNB", b"DSTS", b"DEVS"))
    devid = b"\x0c\x21\x00\x05\x00" in data
    if names and devid:
        return "ASUS keyboard backlight interface present"
    return "absent (" + ("WMNB/DSTS/DEVS found" if names else "no WMNB/DSTS/DEVS") + ", " + ("device id found" if devid else "no device id 0x00050021") + ")"

if __name__ == "__main__":
    bad = 0
    for p in sys.argv[1:]:
        r = check(open(p, "rb").read())
        print(f"{p}: {r}")
        bad += 0 if r.startswith("ASUS") else 1
    sys.exit(1 if bad else 0)
