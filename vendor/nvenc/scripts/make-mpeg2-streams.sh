#!/bin/bash
# make-mpeg2-streams.sh OUTDIR [FFMPEG]: MPEG-1 / MPEG-2 test streams from ffmpeg's encoders (NAME.m2v) with reference
# decodes (NAME.yuv: yuv420p, display order, float AAN IDCT, see fetch-mpeg2-vectors.sh). Runs where ffmpeg is (the guest).
set -euo pipefail
out=${1:?usage: make-mpeg2-streams.sh OUTDIR [FFMPEG]}; F=${2:-ffmpeg}; mkdir -p "$out"
mk() {   # name size frames codec options...
    local n=$1 s=$2 fr=$3 c=$4; shift 4
    [ -s "$out/$n.m2v" ] || "$F" -nostdin -loglevel error -y -f lavfi -i "testsrc2=size=$s:rate=25" -frames:v "$fr" -c:v "$c" "$@" -f "$c" "$out/$n.m2v"
    "$F" -nostdin -loglevel error -y -idct faani -i "$out/$n.m2v" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt yuv420p "$out/$n.yuv"
    echo "$n: $s, $(stat -f %z "$out/$n.m2v" 2>/dev/null || stat -c %s "$out/$n.m2v") bytes"
}
mk mpeg1-cif        352x288   60 mpeg1video -g 15 -bf 2 -b:v 1.5M
mk mpeg1-pal-b3     720x576   60 mpeg1video -g 12 -bf 3 -b:v 5M
mk sd-pal-ildct     720x576   60 mpeg2video -g 12 -bf 2 -b:v 6M -flags +ildct+ilme -vf setfield=tff
mk sd-ntsc-bff      720x480   60 mpeg2video -g 15 -bf 2 -b:v 6M -flags +ildct+ilme -vf setfield=bff
mk hd-1080i         1920x1080 60 mpeg2video -g 12 -bf 2 -b:v 18M -flags +ildct+ilme -vf setfield=tff
mk hd-1080p         1920x1080 60 mpeg2video -g 12 -bf 2 -b:v 15M
mk hd-720p-altscan  1280x720  60 mpeg2video -g 12 -bf 2 -b:v 10M -alternate_scan 1 -intra_vlc 1
mk altscan-only     720x576   30 mpeg2video -g 12 -bf 2 -b:v 6M -alternate_scan 1
mk intravlc-only    720x576   30 mpeg2video -g 12 -bf 2 -b:v 6M -intra_vlc 1
mk dc10-qscale      720x576   30 mpeg2video -g 12 -bf 2 -q:v 3 -qmax 28 -dc 10 -non_linear_quant 1
mk matrices         720x576   30 mpeg2video -g 12 -bf 2 -b:v 6M -intra_matrix 8,9,10,11,12,13,14,15,9,10,11,12,13,14,15,16,10,11,12,13,14,15,16,17,11,12,13,14,15,16,17,18,12,13,14,15,16,17,18,19,13,14,15,16,17,18,19,20,14,15,16,17,18,19,20,21,15,16,17,18,19,20,21,22 -inter_matrix 16,17,18,19,20,21,22,23,17,18,19,20,21,22,23,24,18,19,20,21,22,23,24,25,19,20,21,22,23,24,25,26,20,21,22,23,24,25,26,27,21,22,23,24,25,26,27,28,22,23,24,25,26,27,28,29,23,24,25,26,27,28,29,30
mk long-gop-300     1280x720 300 mpeg2video -g 300 -bf 2 -b:v 8M -sc_threshold 1000000000
mk odd-size         718x478   30 mpeg2video -g 12 -bf 2 -b:v 6M
mk slices-per-row   1920x1080 30 mpeg2video -g 12 -bf 2 -b:v 15M -slices 68
mk uhd-2160p        3840x2160 30 mpeg2video -g 12 -bf 2 -b:v 60M -strict -1
