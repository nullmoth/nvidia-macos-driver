#!/bin/bash
# make-hevc-streams.sh OUTDIR: HEVC test streams for the decoder (M11) from ffmpeg's testsrc2 with libx265, as MP4 (hvc1, with
# composition times, for VideoToolbox) and the same bitstream as Annex B (.hevc), plus a reference decode of each from
# ffmpeg's HEVC decoder (display order, every frame kept). Each exercises something else: B-pyramids and RPS in the slice
# header, RPS in the SPS, open GOP (CRA + RASL), slices, WPP, scaling lists, weighted prediction, lossless (transquant
# bypass), transform skip, AMP, temporal layers, CTB 16/32, HRD/VUI, 10/12-bit, 4:4:4, 4K, odd sizes; 4:2:2 and monochrome
# (refused by the engine).
set -uo pipefail
out=${1:?usage: make-hevc-streams.sh OUTDIR}; mkdir -p "$out"
mk() {   # name size frames pix_fmt x265-params
    local name=$1 size=$2 n=$3 pf=$4 xp=$5
    ffmpeg -nostdin -loglevel error -y -f lavfi -i "testsrc2=size=$size:rate=30" -frames:v "$n" -pix_fmt "$pf" \
        -c:v libx265 -x265-params "log-level=1:$xp" -tag:v hvc1 "$out/$name.mp4" 2>"$out/$name.log" || { echo "$name: FAILED"; grep error "$out/$name.log"; return; }
    ffmpeg -nostdin -loglevel error -y -i "$out/$name.mp4" -c copy -bsf:v hevc_mp4toannexb -f hevc "$out/$name.hevc"
    ffmpeg -nostdin -loglevel error -y -i "$out/$name.hevc" -fps_mode passthrough -noautoscale -f rawvideo -pix_fmt "$pf" "$out/$name.yuv"
    echo "$pf" > "$out/$name.fmt"
    echo "$name: $(stat -c %s "$out/$name.hevc") bytes"
}
mk x265-352x288         352x288   30 yuv420p     "keyint=10:bframes=3"
mk x265-pyramid-640x360 640x360   40 yuv420p     "keyint=40:bframes=8:b-pyramid=1:ref=4"
mk x265-openGOP-640x360 640x360   50 yuv420p     "keyint=16:min-keyint=16:open-gop=1:bframes=4:scenecut=0"
mk x265-closed-640x360  640x360   40 yuv420p     "keyint=16:open-gop=0:bframes=3:scenecut=0"
mk x265-p-only-640x360  640x360   30 yuv420p     "bframes=0:ref=3"
mk x265-ref8-640x360    640x360   30 yuv420p     "bframes=4:ref=8:b-pyramid=1"
mk x265-slices-1280x720 1280x720  20 yuv420p     "slices=4:wpp=1"
mk x265-wpp-1920x1080   1920x1080 15 yuv420p     "wpp=1"
mk x265-nowpp-1280x720  1280x720  15 yuv420p     "wpp=0:sao=0:deblock=-2,2"
mk x265-scaling-640x360 640x360   20 yuv420p     "scaling-list=default"
mk x265-weighted-640x360 640x360  20 yuv420p     "weightp=1:weightb=1:bframes=3"
mk x265-lossless-352x288 352x288  10 yuv420p     "lossless=1"
mk x265-tskip-640x360   640x360   20 yuv420p     "tskip=1:amp=1:rect=1:rd=6:tu-intra-depth=3:tu-inter-depth=3"
mk x265-temporal-640x360 640x360  30 yuv420p     "temporal-layers=3:bframes=4"
mk x265-ctu16-640x360   640x360   20 yuv420p     "ctu=16:min-cu-size=8"
mk x265-ctu32-640x360   640x360   20 yuv420p     "ctu=32:qg-size=16:cbqpoffs=2:crqpoffs=-2"
mk x265-hrd-640x360     640x360   20 yuv420p     "hrd=1:vbv-bufsize=1000:vbv-maxrate=800:aud=1:repeat-headers=1:info=1"
mk x265-optrps-640x360  640x360   40 yuv420p     "opt-ref-list-length-pps=1:multi-pass-opt-rps=0:bframes=3"
mk x265-constrintra-640x360 640x360 20 yuv420p   "constrained-intra=1:strong-intra-smoothing=0:signhide=0"
mk x265-10bit-1280x720  1280x720  20 yuv420p10le "bframes=3"
mk x265-12bit-640x360   640x360   20 yuv420p12le "bframes=3"
mk x265-444-640x360     640x360   10 yuv444p     "bframes=2"
mk x265-422-10bit-640x360 640x360 10 yuv422p10le "bframes=2"
mk x265-444-10bit-640x360 640x360 10 yuv444p10le "bframes=2"
mk x265-444-12bit-640x360 640x360 10 yuv444p12le "bframes=2"
mk x265-gray-640x360    640x360   10 gray        "bframes=2"
mk x265-gray10-640x360  640x360   10 gray10le    "bframes=2"
mk x265-odd-350x286     350x286   20 yuv420p     "bframes=3"
mk x265-3840x2160       3840x2160 10 yuv420p     "preset=ultrafast"
mk x265-10bit-3840x2160 3840x2160 10 yuv420p10le "preset=ultrafast"
