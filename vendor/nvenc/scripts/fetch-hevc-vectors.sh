#!/bin/bash
# fetch-hevc-vectors.sh OUTDIR [NAME...]: the JCT-VC HEVC conformance bitstreams from FFmpeg's FATE suite (Annex B .bit/.bin),
# each with a reference decode from ffmpeg's HEVC decoder (display order, its own pixel format; NAME.fmt says which).
# Without names: every bitstream the FATE directory lists.
set -uo pipefail
out=${1:?usage: fetch-hevc-vectors.sh OUTDIR [NAME...]}; shift; mkdir -p "$out/dl"
B=https://fate-suite.ffmpeg.org/hevc-conformance
names=${*:-$(curl -sSf "$B/" | grep -oE 'href="[^"?/]+\.(bit|bin)"' | sed -E 's/href="(.*)"/\1/')}
total=$(echo $names | wc -w); i=0
for f in $names; do
    i=$((i + 1))
    [ -s "$out/dl/$f" ] || curl -sSf -o "$out/dl/$f" "$B/$f" || { echo "[$i/$total] $f: not found"; rm -f "$out/dl/$f"; continue; }
    n=${f%.*}
    fmt=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt -of csv=p=0 "$out/dl/$f" | head -1 | tr -d ",")
    [ -s "$out/$n.yuv" ] || ffmpeg -nostdin -loglevel error -y -i "$out/dl/$f" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt "$fmt" "$out/$n.yuv"
    echo "$fmt" > "$out/$n.fmt"
    echo "[$i/$total] $n: $fmt, $(stat -c %s "$out/$n.yuv" 2>/dev/null || echo 0) bytes of reference"
done
