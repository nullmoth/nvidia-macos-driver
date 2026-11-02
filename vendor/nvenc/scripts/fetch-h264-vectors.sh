#!/bin/bash
# fetch-h264-vectors.sh OUTDIR: the JVT H.264 conformance bitstreams from FFmpeg's FATE suite (h264-conformance and its
# FRext folder) as NAME.264 (Annex B). 8-bit 4:2:0 only (Ada's NVDEC decodes nothing else in H.264); the others are listed
# as skipped. References are made where ffmpeg runs (OUTDIR/NAME.yuv: yuv420p, display order) unless NOREF=1.
set -uo pipefail
out=${1:?usage: fetch-h264-vectors.sh OUTDIR}; mkdir -p "$out/dl"
B=https://fate-suite.ffmpeg.org/h264-conformance
for dir in "" FRext/; do
    for f in $(curl -sSf "$B/$dir" | grep -oE 'href="[^"?/]+\.(264|jsv|jvt|avc|26l|h264|bit|bin)"' | sed -E 's/href="(.*)"/\1/'); do
        n=$(echo "${dir}${f%.*}" | tr / _)
        [ -s "$out/dl/$n" ] || curl -sSf -o "$out/dl/$n" "$B/$dir$f" || { echo "$n: download failed"; continue; }
        fmt=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt -of csv=p=0 "$out/dl/$n" | head -1 | tr -d ,)
        if [ "$fmt" != yuv420p ] && [ "$fmt" != yuvj420p ]; then echo "$n: $fmt, skipped"; continue; fi
        cp "$out/dl/$n" "$out/$n.264"
        [ "${NOREF:-0}" = 1 ] || ffmpeg -nostdin -loglevel error -y -i "$out/$n.264" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt yuv420p "$out/$n.yuv"
        echo "$n: ok"
    done
done
