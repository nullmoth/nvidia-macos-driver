#!/bin/bash
# decall.sh TOOL EXT DIR: run ~/nvenc-dev/TOOL on every DIR/NAME.EXT against DIR/NAME.yuv (made here with ffmpeg when
# missing) for the bit-exact codecs (H.264, HEVC, VP8, VP9, AV1, VC-1); a stream passes when every frame matches.
T=$1; X=$2; D=$3; pass=0; tot=0; F=/usr/local/bin/ffmpeg
for f in $D/*.$X; do n=${f%.$X}; tot=$((tot+1))
  [ -s $n.yuv ] || $F -nostdin -loglevel quiet -y -i $f -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt yuv420p $n.yuv
  r=$(cd ~/nvenc-dev && ./$T $f $n.yuv 2>&1 | grep -E "RESULT|error|not supported|failed" | tail -1)
  if echo "$r" | grep -qE "RESULT ([0-9]+) shown [a-z]+ \([0-9]+ decoded\), \1 bit-exact|RESULT ([0-9]+) of \2 pictures decoded, \2 bit-exact" && ! echo "$r" | grep -q "the reference has"; then ok=ok; pass=$((pass+1)); else ok=FAIL; fi
  printf "%-4s %-36s %s\n" $ok $(basename $n) "$(echo "$r" | sed 's/RESULT //;s/ ms\/[a-z]*//;s/first difference at/first diff/' | cut -c1-120)"
done
echo "PASS $pass of $tot"
