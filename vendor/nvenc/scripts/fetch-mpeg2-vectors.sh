#!/bin/bash
# fetch-mpeg2-vectors.sh OUTDIR: the ISO/IEC 13818-4 main-profile video conformance bitstreams (ISO's publicly available
# standards site) and FFmpeg's FATE mpeg2 samples, each as an elementary stream NAME.m2v with a reference decode NAME.yuv
# (ffmpeg, yuv420p, display order, float AAN IDCT: MPEG-2 has no normative IDCT, so the reference is the most precise one
# ffmpeg has; T14.01) and NAME.size (WxH). Streams ffmpeg cannot decode as 4:2:0 are left out.
set -uo pipefail
out=${1:?usage: fetch-mpeg2-vectors.sh OUTDIR}; mkdir -p "$out/dl"
ISO=https://standards.iso.org/ittf/PubliclyAvailableStandards/ISO_IEC_13818-4_2004_Conformance_Testing/Video/bitstreams/main-profile
FATE=https://fate-suite.ffmpeg.org/mpeg2
make() {   # name source-file
    local n=$1 src=$2
    if [ "$(head -c 4 "$src" | od -An -tx1 | tr -d ' ')" = 000001b3 ]; then cp "$src" "$out/$n.m2v"
    else ffmpeg -nostdin -loglevel error -y -i "$src" -map 0:v:0 -c:v copy -f mpeg2video "$out/$n.m2v" || { echo "$n: no video"; return; }; fi
    local fmt size
    fmt=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt -of csv=p=0 "$out/$n.m2v" | head -1 | tr -d ,)
    size=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=s=x:p=0 "$out/$n.m2v" | head -1 | sed "s/x$//")
    if [ "$fmt" != yuv420p ]; then echo "$n: $fmt, skipped"; rm -f "$out/$n.m2v"; return; fi
    ffmpeg -nostdin -loglevel error -y -idct faani -i "$out/$n.m2v" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt yuv420p "$out/$n.yuv"
    echo "$size" > "$out/$n.size"
    echo "$n: $size, $(( $(stat -c %s "$out/$n.yuv") / 1000000 )) MB of reference"
}
curl -sSf "$ISO/INDEX.txt.gz" | gunzip | grep '\.gz$' | grep -viE 'trace|readme|decoded|\.log|\.rtf|\.doc|info|vbv|\.yuv|\.d[0-9]' | while read -r p; do
    p=${p#./}; n=$(echo "${p%/*}" | tr '/' '_')
    [ -s "$out/$n.m2v" ] && continue
    curl -sSf -o "$out/dl/$n.gz" "$ISO/$p" || { echo "$n: download failed"; continue; }
    gunzip -c "$out/dl/$n.gz" > "$out/dl/$n" && make "$n" "$out/dl/$n"
done
for f in mpeg2_field_encoding.ts sony-ct3.bs tcela-6.bits dvd_single_frame.vob matrixbench_mpeg2.lq1.mpg xdcam8mp2-1s_small.ts t.mpg; do
    n=fate_${f%.*}; [ -s "$out/$n.m2v" ] && continue
    curl -sSf -o "$out/dl/$f" "$FATE/$f" && make "$n" "$out/dl/$f"
done
