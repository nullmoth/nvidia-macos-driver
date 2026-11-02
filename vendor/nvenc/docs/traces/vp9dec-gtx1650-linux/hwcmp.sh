#!/bin/bash
cd /root/vp9tr
for f in vp90-2-19-skip vp90-2-15-segkey_adpq; do
  ffmpeg -nostdin -v error -y -i $f.ivf -pix_fmt yuv420p -f framemd5 $f.sw.md5
  ffmpeg -nostdin -v error -y -hwaccel cuda -i $f.ivf -pix_fmt yuv420p -f framemd5 $f.hw.md5 2>&1 | tail -3
  echo "$f: frames sw $(grep -vc ^# $f.sw.md5) hw $(grep -vc ^# $f.hw.md5), differing $(diff <(grep -v ^# $f.sw.md5 | cut -d, -f6) <(grep -v ^# $f.hw.md5 | cut -d, -f6) | grep -c '^<'), first: $(diff <(grep -v ^# $f.sw.md5 | cut -d, -f6) <(grep -v ^# $f.hw.md5 | cut -d, -f6) | head -1)"
done
