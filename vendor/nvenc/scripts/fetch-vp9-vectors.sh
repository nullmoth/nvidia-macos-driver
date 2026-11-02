#!/bin/bash
# fetch-vp9-vectors.sh OUTDIR: a selection of libvpx's public VP9 test vectors (show_existing_frame, intra-only frames,
# resizing on inter frames = scaled references, 10-bit, tiles, odd and tiny sizes, segmentation, loop filter deltas,
# big superframes, 4:4:4) as IVF, each with a reference decode from ffmpeg (display order, every frame at its own size:
# -noautoscale) in its own pixel format (NAME.fmt says which).
set -euo pipefail
out=${1:?usage: fetch-vp9-vectors.sh OUTDIR}; mkdir -p "$out/dl"
B=https://storage.googleapis.com/downloads.webmproject.org/test_data/libvpx
for f in vp90-2-02-size-08x08.webm vp90-2-02-size-66x66.webm vp90-2-02-size-130x132.webm vp90-2-03-size-226x226.webm \
         vp90-2-03-deltaq.webm vp90-2-05-resize.ivf vp90-2-07-frame_parallel.webm vp90-2-08-tile-4x1.webm vp90-2-08-tile-4x4.webm \
         vp90-2-09-lf_deltas.webm vp90-2-10-show-existing-frame.webm vp90-2-10-show-existing-frame2.webm vp90-2-11-size-351x287.webm \
         vp90-2-13-largescaling.webm vp90-2-14-resize-fp-tiles-16-8.webm vp90-2-15-segkey.webm vp90-2-15-segkey_adpq.webm \
         vp90-2-16-intra-only.webm vp90-2-17-show-existing-frame.webm vp90-2-18-resize.ivf vp90-2-19-skip.webm \
         vp90-2-20-big_superframe-01.webm vp90-2-20-big_superframe-02.webm vp90-2-21-resize_inter_320x180_5_1-2.webm \
         vp90-2-21-resize_inter_640x360_7_3-4.webm vp90-2-21-resize_inter_1280x720_5_1-2.webm vp90-2-22-svc_1280x720_3.ivf \
         vp91-2-04-yuv444.webm vp92-2-20-10bit-yuv420.webm; do
    [ -s "$out/dl/$f" ] || curl -sSf -o "$out/dl/$f" "$B/$f"
    n=${f%.*}
    ffmpeg -nostdin -loglevel error -y -i "$out/dl/$f" -c copy -copyinkf -f ivf "$out/$n.ivf"
    fmt=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt -of csv=p=0 "$out/dl/$f" | head -1 | tr -d ",")
    ffmpeg -nostdin -loglevel error -y -i "$out/dl/$f" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt "$fmt" "$out/$n.yuv"
    echo "$fmt" > "$out/$n.fmt"
    echo "$n: $fmt, $(stat -c %s "$out/$n.yuv") bytes of reference"
done
