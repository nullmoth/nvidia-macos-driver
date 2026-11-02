#!/bin/bash
# m2all.sh DIR [TOOL EXT]: TOOL (default mpeg2decodeN) on every NAME.EXT (default m2v) with NAME.yuv (ffmpeg faani
# reference), in the guest's ~/nvenc-dev (also mpeg4decodeN m4v / h263). MPEG-1/2/4 and H.263 have no normative IDCT, so each stream gets a baseline: the worst frame of ffmpeg's simple IDCT against faani
# (two correct decoders drifting apart), cached in NAME.base. A stream passes when every frame decodes and our worst
# frame is >= min(55, baseline - 8) dB (the engine's IDCT drifts a little more than ffmpeg's within long GOPs: a sawtooth that
# resets at each I frame).
cd ~/nvenc-dev; pass=0; tot=0; F=/usr/local/bin/ffmpeg; T=${2:-mpeg2decodeN}; X=${3:-m2v}
for f in $1/*.$X; do n=${f%.$X}; tot=$((tot+1))
  # NAME.skip: an output frame ffmpeg's reference does not have (it drops the last anchor before a size change)
  r=$(MPEG2_SKIP_SHOWN=$(cat $n.skip 2>/dev/null || echo -1) ./$T $f $n.yuv 2>&1 | grep -E "RESULT|error" | tail -1)
  w=$(echo "$r" | sed -nE "s/.*worst ([0-9.]+) dB.*/\1/p")
  if [ ! -s $n.base ]; then
    sz=$($F -nostdin -hide_banner -i $f 2>&1 | grep -oE "[0-9]{2,5}x[0-9]{2,5}" | head -1)
    $F -nostdin -v quiet -idct simple -i $f -noautoscale -fps_mode passthrough -f rawvideo -pix_fmt yuv420p -y /tmp/m2s.yuv
    $F -nostdin -v quiet -f rawvideo -pix_fmt yuv420p -s $sz -i /tmp/m2s.yuv -f rawvideo -pix_fmt yuv420p -s $sz -i $n.yuv \
       -lavfi psnr=stats_file=/tmp/m2s.log -f null -
    awk '{for(i=1;i<=NF;i++){split($i,a,":"); if(a[1]~/psnr_[yuv]/){v=(a[2]=="inf")?99:a[2]+0; if(m==""||v<m)m=v}}} END{print (m=="")?99:m}' /tmp/m2s.log > $n.base
  fi
  b=$(cat $n.base)
  ok=$(echo "$r" | grep -q "the reference has\|engine errors\|decode error" && echo 0 || awk -v w="${w:-0}" -v b=$b 'BEGIN{t=b-8; if(t>55)t=55; print (w>=t)?1:0}')
  [ "$ok" = 1 ] && pass=$((pass+1))
  printf "%-4s %-36s base %5.1f  %s\n" $([ "$ok" = 1 ] && echo ok || echo FAIL) $(basename $n) $b \
    "$(echo "$r" | sed 's/RESULT //;s/ ms\/frame//;s/first difference at [-0-9]*, //;s/engine/eng/')"
done
echo "PASS $pass of $tot"
