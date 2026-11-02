#!/bin/bash
# fetch-vc1-vectors.sh OUTDIR: FFmpeg's FATE VC-1 samples (SMPTE conformance streams SA* = Advanced as elementary streams,
# SMM* = Main as RCV, and an interlaced two-MV stream), each with a reference decode NAME.yuv (ffmpeg, yuv420p).
set -euo pipefail
out=${1:?usage: fetch-vc1-vectors.sh OUTDIR}; mkdir -p "$out"
for f in SA00040.vc1 SA00050.vc1 SA10091.vc1 SA10143.vc1 SA20021.vc1 SMM0005.rcv SMM0015.rcv ilaced_twomv.vc1; do
    [ -s "$out/$f" ] || curl -sSf -o "$out/$f" "https://fate-suite.ffmpeg.org/vc1/$f"
    ffmpeg -nostdin -loglevel error -y -i "$out/$f" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt yuv420p "$out/${f%.*}.yuv"
done
