#!/usr/bin/env python3
"""vp9-trace-compare.py parsetest in.ivf: compare tools/vp9parsetest's (VP9TRACE=1) or tools/av1parsetest's (AV1TRACE=1)
header fields with ffmpeg's trace_headers bitstream filter, frame by frame, for every field both print. AV1: a frame
starts at each "Frame Header" / "Frame" OBU of the trace."""
import os, re, subprocess, sys
exe, ivf = sys.argv[1], sys.argv[2]
def frames(lines, start):
    out = []
    for l in lines:
        if start(l): out.append({}); continue
        m = re.search(r"(?:\] \d+ +)?([a-z_0-9\[\]]+)\s+(?:[01]+ )?= (-?\d+)$", l.strip())
        if m and out: out[-1].setdefault(m.group(1), int(m.group(2)))
    return out
ff = subprocess.run(["ffmpeg", "-nostdin", "-hide_banner", "-i", ivf, "-c", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"],
                    capture_output=True, text=True).stderr.splitlines()
ff = frames([l for l in ff if "trace_headers" in l], lambda l: l.rstrip().endswith(("] Frame", "] Frame Header")))
ours = subprocess.run([exe, ivf], capture_output=True, text=True, env=dict(os.environ, VP9TRACE="1", AV1TRACE="1")).stdout.splitlines()
ours = frames(ours, lambda l: l == "FRAME")
bad = checked = 0
for i, (a, b) in enumerate(zip(ours, ff)):
    for k, v in b.items():
        if k in a:
            checked += 1
            if a[k] != v:
                bad += 1
                if bad <= 10: print(f"frame {i}: {k} ours {a[k]} ffmpeg {v}")
print(f"{os.path.basename(ivf)}: {len(ours)} frames (ffmpeg {len(ff)}), {checked} fields compared, {bad} differ")
sys.exit(1 if bad or len(ours) != len(ff) else 0)
