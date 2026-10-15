#!/usr/bin/env python3
"""check_gpu_id_coverage.py proves the field Intel/AMD/NPU PCI ids are bound by the compiled id tables.
Fixtures stand in for the upstream Linux/Mesa sources so the test needs no build host. The negative control
tampers with a table (drops one id / removes the AMD wildcard) and the checker must report the gap."""
import json, pathlib, subprocess, sys, tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
CHECK = ROOT / "tools/check_gpu_id_coverage.py"
fails = []


def arm(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    if not ok:
        fails.append(name)


# field census: one Intel id, one AMD id, one NPU id of each vendor
FIELD = {"intel_gpu": {"8086:46a6": {"machines": 88, "name": "Iris Xe"}, "8086:fd80": {"machines": 1, "name": "bogus"}},
         "amd_gpu": {"1002:164e": {"machines": 77, "name": "Radeon 610M"}},
         "npu": {"8086:ad1d": {"machines": 3, "name": "Arrow Lake NPU"}, "1022:1502": {"machines": 3, "name": "Phoenix NPU"}}}

# minimal upstream-shaped fixtures
PCIIDS = r"""
#define INTEL_ADLP_IDS(MACRO__, ...) \
	MACRO__(0x46A6, ## __VA_ARGS__), \
	MACRO__(0x46A8, ## __VA_ARGS__)
#define INTEL_ALDERLAKE_P_IDS(MACRO__, ...) \
	INTEL_ADLP_IDS(MACRO__, ## __VA_ARGS__)
#define PCI_DEVICE_ID_MTL	0x7d1d
#define PCI_DEVICE_ID_ARL	0xad1d
"""
I915 = "static const struct pci_device_id pciidlist[] = { INTEL_ALDERLAKE_P_IDS(INTEL_VGA_DEVICE, &adl_info), };"
XE = "static const struct pci_device_id xe_pciidlist[] = { };"
AMDGPU = """static const struct pci_device_id pciidlist[] = {
	{ PCI_DEVICE(0x1002, 0x6780), .driver_data = CHIP_TAHITI },
	{ PCI_DEVICE(0x1002, PCI_ANY_ID), .class = PCI_CLASS_DISPLAY_VGA << 8, .driver_data = CHIP_IP_DISCOVERY },
};"""
IVPU = PCIIDS + "\nstatic const struct pci_device_id ivpu_pci_ids[] = { { PCI_DEVICE(PCI_VENDOR_ID_INTEL, PCI_DEVICE_ID_ARL) }, };"
AMDXDNA = "static const struct pci_device_id amdxdna_pci_tbl[] = { { PCI_DEVICE(PCI_VENDOR_ID_AMD, 0x1502) }, };"


def write_tables(d, amdgpu=AMDGPU, i915=I915, pciids=PCIIDS, ivpu=IVPU, amdxdna=AMDXDNA, field=FIELD):
    p = pathlib.Path(d)
    (p / "pciids.h").write_text(pciids); (p / "i915.c").write_text(i915); (p / "xe.c").write_text(XE)
    (p / "amdgpu.c").write_text(amdgpu); (p / "ivpu.c").write_text(ivpu); (p / "amdxdna.c").write_text(amdxdna)
    (p / "field.json").write_text(json.dumps(field))
    return p


def run(p):
    return subprocess.run([sys.executable, "-I", str(CHECK), "--field", str(p / "field.json"),
                           "--pciids", str(p / "pciids.h"), "--i915", str(p / "i915.c"), "--xe", str(p / "xe.c"),
                           "--amdgpu", str(p / "amdgpu.c"), "--ivpu", str(p / "ivpu.c"), "--amdxdna", str(p / "amdxdna.c")],
                          capture_output=True, text=True)


with tempfile.TemporaryDirectory() as d:
    p = write_tables(d)
    r = run(p)
    arm("good tables: every field id is covered, exit 0", r.returncode == 0 and "OK:" in r.stdout, r.stdout.strip())
    arm("the bogus 8086:fd80 is ignored, not reported as a gap", "GAPS" not in r.stdout and "ignored: ['8086:fd80']" in r.stdout)

# negative control 1: Intel id dropped from the macro -> gap, nonzero
with tempfile.TemporaryDirectory() as d:
    p = write_tables(d, pciids=PCIIDS.replace("MACRO__(0x46A6, ## __VA_ARGS__), \\\n\t", ""))
    r = run(p)
    arm("NEGATIVE CONTROL: dropping 8086:46a6 from i915 is reported and exits nonzero",
        r.returncode != 0 and "46a6" in r.stdout and "GAPS" in r.stdout, r.stdout.strip())

# negative control 2: AMD wildcard removed and 0x164e not literally listed -> gap
with tempfile.TemporaryDirectory() as d:
    p = write_tables(d, amdgpu=AMDGPU.replace("{ PCI_DEVICE(0x1002, PCI_ANY_ID), .class = PCI_CLASS_DISPLAY_VGA << 8, .driver_data = CHIP_IP_DISCOVERY },", ""))
    r = run(p)
    arm("NEGATIVE CONTROL: removing the AMD IP-discovery wildcard exposes 1002:164e as a gap",
        r.returncode != 0 and "164e" in r.stdout, r.stdout.strip())

# negative control 3: NPU id dropped from ivpu -> gap
with tempfile.TemporaryDirectory() as d:
    p = write_tables(d, ivpu=IVPU.replace("{ PCI_DEVICE(PCI_VENDOR_ID_INTEL, PCI_DEVICE_ID_ARL) }, ", ""))
    r = run(p)
    arm("NEGATIVE CONTROL: dropping ivpu ARL (8086:ad1d) is reported and exits nonzero",
        r.returncode != 0 and "ad1d" in r.stdout, r.stdout.strip())

print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
