#!/usr/bin/env python3
"""bench-table.py results.txt...: markdown tables from the RESULT lines of scripts/guest/bench-{enc,dec,all}.sh."""
import re, sys
from collections import OrderedDict

ENC_NAMES = OrderedDict([
    ("ours-h264-rt", "ours H.264, real time (P only)"), ("ours-h264", "ours H.264 (2 B, quality)"),
    ("ours-hevc-rt", "ours HEVC, real time"), ("ours-hevc", "ours HEVC (2 B)"), ("ours-hevc10", "ours HEVC Main 10 (2 B)"),
    ("ours-av1-rt", "ours AV1, real time"), ("ours-av1", "ours AV1 (2 B, quality)"), ("ours-av1-10", "ours AV1 10-bit (2 B, quality)"),
    ("apple-h264", "Apple H.264 (software, VideoToolbox)"), ("apple-hevc", "Apple HEVC (software, VideoToolbox)"),
    ("x264-veryfast", "x264 veryfast"), ("x264-medium", "x264 medium"), ("x265-fast", "x265 fast"), ("x265-medium", "x265 medium"),
    ("aom-rt", "libaom AV1 realtime, cpu-used 8")])
DEC_NAMES = OrderedDict([("ours", "ours (NVDEC, VideoToolbox)"), ("ours-cli", "ours (NVDEC, engine time)"), ("nullmoth", "NullMoth's driver"),
                         ("apple-sw", "Apple software (VideoToolbox)"), ("ffmpeg-sw", "ffmpeg software, 16 threads")])
CODEC_NAMES = OrderedDict([("h264", "H.264"), ("hevc", "HEVC"), ("hevc10", "HEVC Main 10"), ("vp9", "VP9"), ("av1", "AV1"),
                           ("mpeg2", "MPEG-2"), ("mpeg4", "MPEG-4 Part 2"), ("vp8", "VP8")])

enc, dec, lat, k8 = OrderedDict(), {}, [], []
source = None
for path in sys.argv[1:]:
    for line in open(path, errors="replace"):
        m = re.match(r"SOURCE (\S+) (\S+) (\d+) frames at (\d+)", line)
        if m: source = (m.group(1), m.group(2), int(m.group(3)), int(m.group(4))); enc.setdefault(source, {}); continue
        if not line.startswith("RESULT "): continue
        f = line.split()
        kv = dict(x.split("=", 1) for x in f if "=" in x)
        if f[1] == "latency": lat.append((f[2], f[3], kv)); continue
        if f[1].startswith("8k-"): k8.append(line.strip()); continue
        if f[1] in ENC_NAMES and source: enc[source][f[1]] = kv; continue
        if f[1] in CODEC_NAMES and len(f) > 3: dec[(f[1], f[2], f[3])] = kv.get("fps")

def num(x, d=1):
    try: return f"{float(x):.{d}f}"
    except (TypeError, ValueError): return "-"

for (name, size, frames, rate), r in enc.items():
    pretty = re.sub(r"(_?\d+p?)?(_60)?$", "", name).replace("crowd", "crowd_run").replace("park", "park_joy")
    print(f"\n#### {pretty} {size}, {frames} frames, target {rate / 1e6:g} Mbit/s\n")
    print("| encoder | fps | Mbit/s | PSNR-Y dB | VMAF |\n|---|---:|---:|---:|---:|")
    for k, label in ENC_NAMES.items():
        if k in r:
            v = r[k]
            print(f"| {label} | {num(v.get('fps'), 0)} | {num(float(v['kbps']) / 1000 if v.get('kbps') else None, 2)} | {num(v.get('psnr'), 2)} | {num(v.get('vmaf'), 1)} |")

for size in sorted({k[2] for k in dec}, key=lambda s: int(s.split("x")[0])):
    print(f"\n#### Decode {size} (frames per second)\n")
    cols = [d for d in DEC_NAMES if any(k[1] == d and k[2] == size for k in dec)]
    print("| codec | " + " | ".join(DEC_NAMES[c] for c in cols) + " |\n|---|" + "---:|" * len(cols))
    for c, label in CODEC_NAMES.items():
        if not any(k[0] == c and k[2] == size for k in dec): continue
        print(f"| {label} | " + " | ".join(num(dec.get((c, d, size)), 0) for d in cols) + " |")

if lat:
    print("\n#### Real-time encode latency (encodeFrame to encoded frame, libnvencvt statistics)\n")
    print("| codec | size | latency ms | engine ms |\n|---|---|---:|---:|")
    for c, size, kv in lat:
        print(f"| {dict(avc1='H.264', hvc1='HEVC', av01='AV1').get(c, c)} | {size} | {num(kv.get('entry_emit_ms'), 2)} | {num(kv.get('engine_ms'), 2)} |")
if k8:
    print("\n#### 8K\n")
    for l in k8: print("    " + l)
