#!/usr/bin/env python3
"""check_ttu_abi.py - ttu_abi.h against recorded device output.

The three data/*.bin files are the 32-lane results the RTX 5060 returned for the ray corpus in data/ray_cases.json: against a
captured structure, against a freshly built bottom-level structure, and against a freshly built bottom + top pair. The
expected values in the corpus come from a separate CPU ray/triangle test over the same 16 triangles, not from the hardware.

    python3 nvk/hwrt/check_ttu_abi.py          # exit 1 on any failure; also compiles and runs test_ttu_abi.c
"""
import json, math, os, struct, subprocess, sys, tempfile

here = os.path.dirname(os.path.abspath(__file__))
CASES = {"results_blas_captured.bin": "ray_cases.json", "results_blas_fresh.bin": "ray_cases_fresh.json",
         "results_tlas_fresh.bin": "ray_cases_fresh.json"}
SETS = list(CASES)


def cases_for(name):
    return json.load(open(os.path.join(here, "data", CASES[name])))


def decode(r, sign_mask=True, hit_rule="v-sign", geo_mask=True, front_rule="u-sign"):
    """Python port of ttu_decode; the keyword arguments exist so the controls can break one rule at a time."""
    hit = (r[15] & 0x80000000) == 0 if hit_rule == "v-sign" else (r[15] & 0x80000000) != 0
    if not hit:
        return {"hit": False}
    m = 0x7fffffff if sign_mask else 0xffffffff
    f = lambda u: struct.unpack("<f", struct.pack("<I", u & m))[0]
    k = r[12] >> 29
    pbits = 28 - 4 * k
    return {"hit": True, "t": f(r[13]), "u": f(r[14]), "v": f(r[15]), "primitive": r[12] & ((1 << pbits) - 1),
            "geometry": ((r[12] & 0x0fffffff) if geo_mask else r[12]) >> pbits, "instance": r[5] - 1, "front": ((r[14] >> 31) == 0) if front_rule == "u-sign" else ((r[14] >> 31) != 0)}


def compare(rows, cases):
    bad = []
    for lane, (c, row) in enumerate(zip(cases, rows)):
        e = c["expected"]
        if row["hit"] != e["hit"]:
            bad.append((lane, c["label"], "hit", row["hit"], e["hit"]))
            continue
        if not e["hit"]:
            continue
        for k in ("t", "u", "v"):
            if not math.isclose(row[k], e[k], rel_tol=1e-5, abs_tol=1e-6):
                bad.append((lane, c["label"], k, row[k], e[k]))
        for k in ("primitive", "geometry"):
            if row[k] != e[k]:
                bad.append((lane, c["label"], k, row[k], e[k]))
    return bad


def load(name):
    d = open(os.path.join(here, "data", name), "rb").read()
    assert len(d) == 32 * 96, (name, len(d))
    return [struct.unpack_from("<16I", d, lane * 96 + 32) for lane in range(32)]


fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (("  " + detail) if detail and not ok else ""))
    fails += (not ok)


for n in sorted(set(CASES.values())):
    cs = json.load(open(os.path.join(here, "data", n)))
    hits = sum(c["expected"]["hit"] for c in cs)
    arm(f"{n}: 24 hits and 8 misses (a corpus of one kind proves nothing)", hits == 24 and len(cs) - hits == 8, f"{hits} hits of {len(cs)}")

for s in SETS:
    rows = [decode(r) for r in load(s)]
    bad = compare(rows, cases_for(s))
    arm(f"{s}: all 32 lanes match the CPU test (hit, t, u, v, primitive, geometry)", not bad, str(bad[:3]))
    # every triangle in both corpora faces +z, so a ray with direction z > 0 meets its back side
    bad_f = [c["label"] for c, r in zip(cases_for(s), rows) if r["hit"] and r["front"] != (c["direction"][2] < 0)]
    both = {c["direction"][2] < 0 for c, r in zip(cases_for(s), rows) if r["hit"]}
    arm(f"{s}: front/back face (sign of word 14) matches the geometry for every hit, and both faces occur",
        not bad_f and both == {True, False}, str(bad_f[:3]) + str(both))
    inst = sorted(set(r["instance"] for r in rows if r["hit"]))
    arm(f"{s}: every hit reports one instance value", len(inst) == 1, str(inst))

# negative controls: break exactly one rule; the same comparison must then fail
for name, kw in (("keeping the sign bit of u", {"sign_mask": False}), ("reading the hit sign backwards", {"hit_rule": "inverted"}),
                 ("not masking the geometry field", {"geo_mask": False})):
    bad = compare([decode(r, **kw) for r in load(SETS[0])], cases_for(SETS[0]))
    arm(f"control: {name} is caught", bool(bad), "the broken decode still matched, so the check cannot tell")

bad = [1 for c, r in zip(cases_for(SETS[0]), [decode(x, front_rule="inv") for x in load(SETS[0])]) if r["hit"] and r["front"] != (c["direction"][2] < 0)]
arm("control: reading the face bit backwards is caught", bool(bad), "the inverted face rule still matched")

# the C header must decode exactly what the python port decodes
with tempfile.TemporaryDirectory() as tmp:
    exe = os.path.join(tmp, "t")
    r = subprocess.run(["cc", "-O1", "-Wall", "-Werror", "-I" + here, "-o", exe, os.path.join(here, "test_ttu_abi.c")],
                       capture_output=True, text=True)
    arm("test_ttu_abi.c compiles without warnings", r.returncode == 0, r.stderr[-400:])
    if r.returncode == 0:
        out = []
        for s in SETS:
            out = subprocess.run([exe, os.path.join(here, "data", s)], capture_output=True, text=True).stdout.splitlines()
            ok = True
            for lane, line in enumerate(out[:32]):
                p = line.split()
                py = decode(load(s)[lane])
                if int(p[1]) != int(py["hit"]):
                    ok = False
                elif py["hit"]:
                    ok &= all(math.isclose(float(p[i]), py[k], rel_tol=1e-6) for i, k in ((2, "t"), (3, "u"), (4, "v")))
                    ok &= (int(p[5]), int(p[6]), int(p[7]), int(p[8])) == (py["primitive"], py["geometry"], py["instance"], int(py["front"]))
            arm(f"C decode == python decode on {s}", ok)
        pk = out[32].split()
        arm("ttu_pack puts mask, tmin, tmax, root and the kind bits where the proof program does",
            pk[1:] == ["00ff0000", "00000000", "41200000", "00ff0000", "23456780", "10000001"], " ".join(pk))
print("%d arms failed" % fails)
sys.exit(1 if fails else 0)
