#!/bin/bash
# make-vp9-streams.sh OUTDIR: VP9 test streams (IVF) for the decoder (M10) from ffmpeg's testsrc2 with libvpx-vp9, plus a
# reference decode of each (I420 / yuv420p10le, display order) from ffmpeg's own VP9 decoder. Each stream exercises
# something else: alt-ref (hidden frames + superframes), tiles, error resilience, lossless, frame-parallel mode,
# segmentation (cyclic refresh), tile rows, 4K, 10/12-bit (profile 2, 4K and a width that is not a multiple of 64), odd sizes.
set -euo pipefail
out=${1:?usage: make-vp9-streams.sh OUTDIR}; mkdir -p "$out"
# frame-parallel mode (no backward adaptation) is ffmpeg's libvpx default; every stream but fpd-* turns it off.
# PASSES=2 streams are two-pass (libvpx makes alt-ref frames, hidden + superframes, only then).
mk() {   # name size frames pix_fmt [libvpx options...]
    local name=$1 size=$2 n=$3 pf=$4; shift 4
    local fp=(-frame-parallel 0); [[ $name == fpd-* ]] && fp=()
    for p in $(seq 1 "${PASSES:-1}"); do
        local pass=(); [ "${PASSES:-1}" = 2 ] && pass=(-pass "$p" -passlogfile "$out/.pass-$name")
        ffmpeg -nostdin -loglevel error -y -f lavfi -i "testsrc2=size=$size:rate=30" -frames:v "$n" -pix_fmt "$pf" \
            -c:v libvpx-vp9 "${fp[@]}" "${pass[@]}" "$@" -f ivf "$out/$name.ivf"
    done
    rm -f "$out/.pass-$name"*
    ffmpeg -nostdin -loglevel error -y -i "$out/$name.ivf" -fps_mode passthrough -f rawvideo -pix_fmt "$pf" "$out/$name.yuv"
    echo "$name: $(stat -c %s "$out/$name.ivf") bytes"
}
PASSES=2 mk altref-352x288    352x288   60 yuv420p -b:v 400k -auto-alt-ref 1 -lag-in-frames 16
PASSES=2 mk altref-1920x1080  1920x1080 60 yuv420p -b:v 3M -auto-alt-ref 1 -lag-in-frames 16 -tile-columns 1
mk tiles-1280x720    1280x720  60 yuv420p -b:v 2M -tile-columns 2 -row-mt 1
mk errres-640x360    640x360   45 yuv420p -b:v 800k -error-resilient 1
mk lossless-320x240  320x240   10 yuv420p -lossless 1
mk fpd-640x360       640x360   45 yuv420p -b:v 800k -frame-parallel 1
mk seg-640x360       640x360   60 yuv420p -b:v 600k -deadline realtime -cpu-used 8 -aq-mode 3
mk rows-1920x1080    1920x1080 30 yuv420p -b:v 4M -tile-columns 2 -tile-rows 2
mk uhd-3840x2160     3840x2160 30 yuv420p -b:v 12M -deadline realtime -cpu-used 8 -tile-columns 4
mk p2-10bit-1280x720 1280x720  30 yuv420p10le -b:v 2M -profile:v 2
mk p2-12bit-640x360  640x360   30 yuv420p12le -b:v 1M -profile:v 2
mk p2-10bit-3840x2160 3840x2160 20 yuv420p10le -b:v 12M -profile:v 2 -deadline realtime -cpu-used 8 -tile-columns 4
mk p2-10bit-1366x768 1366x768  30 yuv420p10le -b:v 2M -profile:v 2
mk odd-351x287       351x287   30 yuv420p -b:v 400k
