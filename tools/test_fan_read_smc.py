#!/usr/bin/env python3
"""fan_read_smc.py decodes the SMC fan types and never turns a failed read into a reading. No hardware: a fake SMC."""
import importlib.util, pathlib, struct, sys

spec = importlib.util.spec_from_file_location("f", pathlib.Path(__file__).with_name("fan_read_smc.py"))
f = importlib.util.module_from_spec(spec); spec.loader.exec_module(f)
fails = 0
def check(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else "")); fails += not ok

class Fake:
    def __init__(self, keys): self.keys = keys
    def read(self, k):
        if k not in self.keys: raise KeyError(f"no {k}")
        return self.keys[k]
    def close(self): pass

check("flt (Apple silicon, VirtualSMC) decodes", f.decode("flt ", struct.pack("<f", 1234.5)) == 1234.5)
check("fpe2 (older Intel Macs) decodes", f.decode("fpe2", struct.pack(">H", 1800 * 4)) == 1800.0)
check("ui8 fan count decodes", f.decode("ui8 ", b"\x03") == 3)
try: f.decode("sp78", b"\x00\x00"); check("an unknown type is refused, not guessed", False)
except ValueError: check("an unknown type is refused, not guessed", True)
two = Fake({"FNum": ("ui8 ", b"\x02"), "F0Ac": ("flt ", struct.pack("<f", 900.0)), "F1Ac": ("flt ", struct.pack("<f", 1500.0))})
check("two fans read as two speeds", f.fans(two) == [900.0, 1500.0], f.fans(two))
missing = Fake({"FNum": ("ui8 ", b"\x02"), "F0Ac": ("flt ", struct.pack("<f", 900.0))})
try: f.fans(missing); check("a fan key that cannot be read stops the read (no partial list)", False)
except KeyError: check("a fan key that cannot be read stops the read (no partial list)", True)
print("FAILED %d" % fails if fails else "PASS all arms"); sys.exit(1 if fails else 0)
