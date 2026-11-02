#!/usr/bin/env python3
"""hevc-trace-compare.py hevcparsetest in.hevc: compare tools/hevcparsetest's (HEVCTRACE=1) SPS, PPS and slice segment
header fields with ffmpeg's trace_headers bitstream filter, NAL unit by NAL unit, for every field both print. Fields
ours prints with "@bit" also compare the bit position (RBSP bits from the start of the NAL unit); "_end" is the end of
the slice segment header (after byte_alignment())."""
import os, re, subprocess, sys
exe, f = sys.argv[1], sys.argv[2]
NAMES = {"Sequence Parameter Set": "SPS", "Picture Parameter Set": "PPS", "Slice Segment Header": "SLICE"}
ff = subprocess.run(["ffmpeg", "-nostdin", "-hide_banner", "-i", f, "-c", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"],
                    capture_output=True, text=True).stderr.splitlines()
ffs, packets, last = [], False, None
for l in ff:
    if "trace_headers" not in l: continue
    l = re.sub(r"^\[trace_headers @ [0-9a-fx]+\] ", "", l.rstrip())
    if l.startswith("Packet:"): packets = True; continue
    if not packets: continue                                  # the extradata repeats the parameter sets
    if not re.match(r"\d", l):
        last = None
        if l in NAMES: ffs.append((NAMES[l], {})); last = ffs[-1][1]
        continue
    m = re.match(r"(\d+)\s+([A-Za-z_0-9\[\]]+)\s+[01]+ = (-?\d+)$", l)
    if m and last is not None:
        pos, k, v = int(m.group(1)), m.group(2), int(m.group(3))
        if k == "nuh_layer_id" and v: ffs.pop(); last = None; continue   # ours reads the base layer only
        last.setdefault(k, (pos, v)); last["_end"] = (0, (pos + 8) // 8 * 8)
ours, cur = [], None
for l in subprocess.run([exe, f], capture_output=True, text=True, env=dict(os.environ, HEVCTRACE="1")).stdout.splitlines():
    if l.startswith("SECTION "): ours.append((l[8:], {})); cur = ours[-1][1]; continue
    m = re.match(r"([A-Za-z_0-9\[\]]+)(?: = (-?\d+))?(?: @(\d+))?$", l)
    if m and cur is not None: cur[m.group(1)] = (int(m.group(3)) if m.group(3) else None, int(m.group(2)) if m.group(2) else None)
bad = checked = 0
for i, ((na, a), (nb, b)) in enumerate(zip(ours, ffs)):
    if na != nb:
        print(f"NAL {i}: ours {na} ffmpeg {nb}"); bad += 1; break
    for k, (pos, v) in a.items():
        if k == "_end" and nb != "SLICE": continue
        if k not in b: continue
        checked += 1
        fp, fv = b[k]
        if (v is not None and v != fv) or (pos is not None and pos != fp):
            bad += 1
            if bad <= 10: print(f"NAL {i} {na}: {k} ours {v}@{pos} ffmpeg {fv}@{fp}")
print(f"{os.path.basename(f)}: {len(ours)} NAL units (ffmpeg {len(ffs)}), {checked} fields compared, {bad} differ")
sys.exit(1 if bad or len(ours) != len(ffs) else 0)
