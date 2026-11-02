#!/bin/bash
# fetch-vp8-vectors.sh OUTDIR: the VP8 test vectors (vp80-00-comprehensive-001..017) and FATE's VP8 samples
# (sintel-signbias, frame_size_change) as IVF, each with a reference decode NAME.yuv (ffmpeg, yuv420p, shown frames).
set -euo pipefail
out=${1:?usage: fetch-vp8-vectors.sh OUTDIR}; mkdir -p "$out/dl"
for i in $(seq -w 1 17); do f=vp80-00-comprehensive-0$i.ivf; [ -s "$out/$f" ] || curl -sSf -o "$out/$f" "https://fate-suite.ffmpeg.org/vp8-test-vectors-r1/$f"; done
[ -s "$out/sintel-signbias.ivf" ] || curl -sSf -o "$out/sintel-signbias.ivf" https://fate-suite.ffmpeg.org/vp8/sintel-signbias.ivf
[ -s "$out/dl/frame_size_change.webm" ] || curl -sSf -o "$out/dl/frame_size_change.webm" https://fate-suite.ffmpeg.org/vp8/frame_size_change.webm
[ -s "$out/frame_size_change.ivf" ] || ffmpeg -nostdin -loglevel error -y -i "$out/dl/frame_size_change.webm" -c:v copy "$out/frame_size_change.ivf"
for f in "$out"/*.ivf; do ffmpeg -nostdin -loglevel error -y -i "$f" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt yuv420p "${f%.ivf}.yuv"; done
ls "$out"/*.ivf | wc -l
