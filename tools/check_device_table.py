#!/usr/bin/env python3
"""check_device_table.py <ogkm-README.md>: prove the installer's device table against NVIDIA's open-gpu-kernel-modules
supported-GPU list (the README table of the same 620.70.06 tree the kernel side is built from).

Arms (each prints PASS or FAIL):
  - every id the installer carries is in NVIDIA's list;
  - every id in NVIDIA's list that the installer leaves out is a data-centre or mining part with no display output
    (the allowlist below is by product name, so a new GeForce / RTX / Quadro / T-series row can never be dropped silently);
  - every carried id falls in exactly one generation row of plugin/nvmtl_dlss.c (the table the DLSS, SM-target and
    architecture choices key on), so no card reaches a per-generation path with no row;
  - the installer's table is sorted, unique, and the count in docs/CARD-SUPPORT.md matches;
  - an optional second file (the 1401 / Mac app nvidia_gsp_ids.json) lists the same ids with NVIDIA's names.
Exit status 0 only when every arm passes. The README is NVIDIA's (MIT) and is read, never copied into this tree."""
import json, re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# product-name patterns of parts that have no display output (compute, mining, vGPU servers, SoCs)
HEADLESS = re.compile(r"\b(A100|A800|A30|A10|A10G|A10M|A16|A2|AX800|H100|H200|H20|H800|GH200|GB200|GB300|B200|B300|L4|L2|L20|L40S|"
                      r"PG\d+|CMP \d+HX|Tesla|DRIVE|VR NVL72|DGX Display)\b|Matrox")

def readme_ids(path):
    ids = {}
    for name, i in re.findall(r"^\| (.+?)\s+\| ([0-9A-F]{4})(?: [0-9A-F]{4} [0-9A-F]{4})?\s*\|", Path(path).read_text(), re.M):
        ids.setdefault(i, set()).add(name.strip())
    return ids

def installer_ids():
    s = (ROOT / "package/install.sh").read_text()
    return re.search(r'SUPPORTED_IDS="\n(.*?)\n"', s, re.S).group(1).split()

def dlss_rows():
    s = (ROOT / "plugin/nvmtl_dlss.c").read_text()
    return [(int(a, 16), int(b, 16), n) for a, b, n in re.findall(r"\{\s*0x([0-9a-fA-F]{4}),\s*0x([0-9a-fA-F]{4}),\s*\d+,\s*0x[0-9a-fA-F]+,\s*\"([^\"]+)\"\s*\}", s)]

fails = 0
def arm(what, ok, detail=""):
    global fails
    print(("PASS  " if ok else "FAIL  ") + what + (("  " + detail) if detail and not ok else ""))
    fails += 0 if ok else 1

def main(argv):
    nv = readme_ids(argv[1]); mine = installer_ids()
    arm("NVIDIA's list was read (more than 250 ids)", len(nv) > 250, str(len(nv)))
    arm("the installer table is sorted and unique, 4 upper-case hex digits each",
        mine == sorted(set(mine)) and all(re.fullmatch(r"[0-9A-F]{4}", k) for k in mine))
    arm("every installer id is in NVIDIA's 620.70.06 list", not [i for i in mine if i not in nv], str([i for i in mine if i not in nv]))
    left = [(i, sorted(nv[i])) for i in sorted(nv) if i not in mine]
    wrong = [(i, n) for i, n in left if not all(HEADLESS.search(x) for x in n)]
    arm("every id left out is a headless data-centre / mining part (%d left out)" % len(left), not wrong, str(wrong))
    rows = dlss_rows()
    arm("plugin/nvmtl_dlss.c carries the generation rows (6)", len(rows) == 6, str(len(rows)))
    nogen = [i for i in mine if sum(lo <= int(i, 16) <= hi for lo, hi, _ in rows) != 1]
    arm("every installer id falls in exactly one generation row", not nogen, str(nogen[:8]))
    cnt = re.search(r"carries (\d+) distinct", (ROOT / "docs/CARD-SUPPORT.md").read_text())
    arm("docs/CARD-SUPPORT.md states the count (%d)" % len(mine), bool(cnt) and int(cnt.group(1)) == len(mine))
    if len(argv) > 2:
        j = json.load(open(argv[2]))["ids"]
        arm("the app's id list equals the installer's", sorted(j) == mine, str(sorted(set(j) ^ set(mine))))
        arm("every app name is one of NVIDIA's names for that id", all(v in nv.get(k, ()) for k, v in j.items()),
            str([k for k, v in j.items() if v not in nv.get(k, ())]))
    per = {}
    for i in mine:
        for lo, hi, n in rows:
            if lo <= int(i, 16) <= hi: per[n] = per.get(n, 0) + 1
    print("ids per generation row:", per)
    print("FAILED %d" % fails if fails else "PASS all arms")
    return 1 if fails else 0

if __name__ == "__main__":
    if len(sys.argv) < 2: sys.exit(__doc__)
    sys.exit(main(sys.argv))
