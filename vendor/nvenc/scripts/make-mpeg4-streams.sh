#!/bin/bash
# make-mpeg4-streams.sh OUTDIR [FFMPEG]: MPEG-4 Part 2 (NAME.m4v) and H.263 (NAME.h263) test streams from ffmpeg's encoders,
# with reference decodes (NAME.yuv: yuv420p, display order, float AAN IDCT). Runs where ffmpeg is (the guest).
# Encoded with 4 threads: ffmpeg's mpeg4 encoder with B-frames and >= 8 slice threads writes B-VOPs that NVDEC decodes
# differently from ffmpeg (docs/tasks.md T14.02).
set -euo pipefail
out=${1:?usage: make-mpeg4-streams.sh OUTDIR [FFMPEG]}; F=${2:-ffmpeg}; mkdir -p "$out"
mk() {   # name ext size frames codec options...
    local n=$1 x=$2 s=$3 fr=$4 c=$5; shift 5
    local fmt=m4v; [ "$x" = h263 ] && fmt=h263
    [ -s "$out/$n.$x" ] || "$F" -nostdin -loglevel error -y -f lavfi -i "testsrc2=size=$s:rate=25" -frames:v "$fr" -c:v "$c" -threads 4 "$@" -f $fmt "$out/$n.$x"
    "$F" -nostdin -loglevel error -y -idct faani -i "$out/$n.$x" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt yuv420p "$out/$n.yuv"
    echo "$n: $s"
}
mk sp-cif          m4v  352x288   60 mpeg4 -g 30 -bf 0 -b:v 1M
mk asp-b2          m4v  720x576   60 mpeg4 -g 12 -bf 2 -b:v 4M
mk asp-b3-hq       m4v  720x576   30 mpeg4 -g 12 -bf 3 -q:v 2
mk qpel            m4v  720x576   30 mpeg4 -g 12 -bf 2 -b:v 4M -flags +qpel
mk mv4             m4v  720x576   30 mpeg4 -g 12 -bf 2 -b:v 4M -flags +mv4
mk mpegquant       m4v  720x576   30 mpeg4 -g 12 -bf 2 -b:v 4M -mpeg_quant 1
mk matrices        m4v  720x576   30 mpeg4 -g 12 -bf 2 -b:v 4M -mpeg_quant 1 -intra_matrix 8,9,10,11,12,13,14,15,9,10,11,12,13,14,15,16,10,11,12,13,14,15,16,17,11,12,13,14,15,16,17,18,12,13,14,15,16,17,18,19,13,14,15,16,17,18,19,20,14,15,16,17,18,19,20,21,15,16,17,18,19,20,21,22 -inter_matrix 16,17,18,19,20,21,22,23,17,18,19,20,21,22,23,24,18,19,20,21,22,23,24,25,19,20,21,22,23,24,25,26,20,21,22,23,24,25,26,27,21,22,23,24,25,26,27,28,22,23,24,25,26,27,28,29,23,24,25,26,27,28,29,30
mk interlaced      m4v  720x576   30 mpeg4 -g 12 -bf 2 -b:v 6M -flags +ildct+ilme
mk interlaced-altv m4v  720x576   30 mpeg4 -g 12 -bf 2 -b:v 6M -flags +ildct+ilme -alternate_scan 1
mk partitioned     m4v  720x576   30 mpeg4 -g 12 -bf 0 -b:v 4M -data_partitioning 1 -ps 1000
mk resync          m4v  720x576   30 mpeg4 -g 12 -bf 2 -b:v 4M -ps 500
mk acpred-off      m4v  720x576   30 mpeg4 -g 12 -bf 2 -b:v 4M -flags +aic
mk odd-size        m4v  718x478   30 mpeg4 -g 12 -bf 2 -b:v 4M
mk hd-1080p        m4v  1920x1080 30 mpeg4 -g 12 -bf 2 -b:v 15M
mk h263-cif        h263 352x288   60 h263  -g 30 -b:v 1M
mk h263-qcif       h263 176x144   60 h263  -g 30 -b:v 256k
mk h263-4cif       h263 704x576   30 h263  -g 30 -b:v 4M
