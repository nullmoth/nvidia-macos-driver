#!/bin/bash
# make-av1-streams.sh OUTDIR: AV1 test streams (IVF) for the decoder (M9) from ffmpeg's testsrc2 with libaom, SVT-AV1
# and rav1e, plus a reference decode of each from dav1d (display order, every frame kept, each at its own size).
# Each exercises something else: hidden frames / show_existing_frame (libaom alt-ref), tiles, 10-bit, screen content
# (palette, intra block copy), superres, film grain, error resilience, 4K, odd sizes, other encoders' choices.
set -euo pipefail
out=${1:?usage: make-av1-streams.sh OUTDIR}; mkdir -p "$out"
mk() {   # name size frames pix_fmt encoder [options...]
    local name=$1 size=$2 n=$3 pf=$4 enc=$5; shift 5
    ffmpeg -nostdin -loglevel error -y -f lavfi -i "testsrc2=size=$size:rate=30" -frames:v "$n" -pix_fmt "$pf" \
        -c:v "$enc" "$@" -f ivf "$out/$name.ivf"
    ffmpeg -nostdin -loglevel error -y -c:v libdav1d -i "$out/$name.ivf" -fps_mode passthrough -noautoscale -f rawvideo -pix_fmt "$pf" "$out/$name.yuv"
    echo "$name: $(stat -c %s "$out/$name.ivf") bytes"
}
mk aom-352x288        352x288   30 yuv420p     libaom-av1 -cpu-used 6 -b:v 300k
mk aom-altref-640x360 640x360   40 yuv420p     libaom-av1 -cpu-used 4 -b:v 500k -lag-in-frames 19 -auto-alt-ref 1
mk aom-tiles-1920x1080 1920x1080 20 yuv420p    libaom-av1 -cpu-used 6 -b:v 3M -tiles 4x2 -row-mt 1
mk aom-10bit-1280x720 1280x720  20 yuv420p10le libaom-av1 -cpu-used 6 -b:v 1M
mk aom-screen-640x360 640x360   20 yuv420p     libaom-av1 -cpu-used 6 -b:v 400k -tune-content screen
mk aom-errres-640x360 640x360   20 yuv420p     libaom-av1 -cpu-used 6 -b:v 400k -error-resilience 1
mk svt-superres-640x360 640x360 20 yuv420p     libsvtav1 -preset 10 -b:v 300k -svtav1-params superres-mode=1:superres-denom=12
mk aom-grain-640x360  640x360   20 yuv420p     libaom-av1 -cpu-used 6 -b:v 400k -denoise-noise-level 20
mk aom-odd-351x287    351x287   20 yuv420p     libaom-av1 -cpu-used 6 -b:v 300k
mk svt-1920x1080      1920x1080 30 yuv420p     libsvtav1 -preset 10 -b:v 2M
mk svt-10bit-3840x2160 3840x2160 10 yuv420p10le libsvtav1 -preset 12 -b:v 8M
mk svt-grain-1280x720 1280x720  20 yuv420p     libsvtav1 -preset 10 -svtav1-params film-grain=8
mk rav1e-640x360      640x360   30 yuv420p     librav1e -speed 10 -b:v 500k
mk rav1e-tiles-1920x1080 1920x1080 20 yuv420p  librav1e -speed 10 -b:v 3M -tiles 4
