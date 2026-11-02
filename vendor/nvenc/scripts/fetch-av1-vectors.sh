#!/bin/bash
# fetch-av1-vectors.sh OUTDIR: a selection of libaom's public AV1 test vectors as IVF, each with a reference decode from
# dav1d (display order, every frame at its own size, its own pixel format; NAME.fmt says which). Missing names are skipped.
set -uo pipefail
out=${1:?usage: fetch-av1-vectors.sh OUTDIR}; mkdir -p "$out/dl"
B=https://storage.googleapis.com/aom-test-data
names="av1-1-b8-01-size-16x16.ivf av1-1-b8-01-size-16x18.ivf av1-1-b8-01-size-18x16.ivf av1-1-b8-01-size-34x34.ivf
  av1-1-b8-01-size-66x66.ivf av1-1-b8-01-size-196x196.ivf av1-1-b8-01-size-202x210.ivf av1-1-b8-01-size-226x226.ivf
  av1-1-b8-02-allintra.ivf av1-1-b8-03-sizedown.mkv av1-1-b8-03-sizeup.mkv av1-1-b8-04-cdfupdate.ivf av1-1-b8-05-mv.ivf
  av1-1-b8-06-mfmv.ivf av1-1-b8-22-svc-L1T2.ivf av1-1-b8-22-svc-L2T1.ivf av1-1-b8-22-svc-L2T2.ivf av1-1-b8-23-film_grain-50.ivf
  av1-1-b10-00-quantizer-00.ivf av1-1-b10-00-quantizer-31.ivf av1-1-b10-00-quantizer-63.ivf av1-1-b10-23-film_grain-50.ivf
  av1-1-b8-24-monochrome.ivf av1-1-b10-24-monochrome.ivf av1-1-b8-16-intra_only-intrabc-extreme-dv.ivf av1-1-b8-16-intra-only.ivf"
for f in $names; do
    [ -s "$out/dl/$f" ] || curl -sSf -o "$out/dl/$f" "$B/$f" 2>/dev/null || { echo "$f: not found"; rm -f "$out/dl/$f"; continue; }
    n=${f%.*}
    ffmpeg -nostdin -loglevel error -y -i "$out/dl/$f" -c copy -copyinkf -f ivf "$out/$n.ivf" || { echo "$f: remux failed"; continue; }
    fmt=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt -of csv=p=0 "$out/dl/$f" | head -1 | tr -d ",")
    ffmpeg -nostdin -loglevel error -y -c:v libdav1d -i "$out/dl/$f" -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt "$fmt" "$out/$n.yuv"
    echo "$fmt" > "$out/$n.fmt"
    echo "$n: $fmt, $(stat -c %s "$out/$n.yuv") bytes of reference"
done
