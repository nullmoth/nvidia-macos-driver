#!/bin/bash
# trace.sh NAME FRAMES: NVIDIA's VP9 decode of NAME.ivf through the tracer
cd /root/vp9tr; n=$1; fr=$2; rm -rf t-$n; mkdir -p t-$n
LD_PRELOAD=/root/vp9tr/nvtrace.so NVTRACE_OUT=/root/vp9tr/t-$n/log.txt NVTRACE_DIR=/root/vp9tr/t-$n NVTRACE_ONLY=404,408,5c0,5c4,5c8,5cc  \
  ffmpeg -nostdin -v warning -y -hwaccel cuda -i $n.ivf -frames:v $fr -pix_fmt yuv420p -f framemd5 t-$n/out.md5 2>&1 | tail -3
echo "execs: $(grep -c "==== EXECUTE" t-$n/log.txt), files $(ls t-$n | wc -l), $(du -sh t-$n | cut -f1)"
grep -m1 -A40 "==== EXECUTE" t-$n/log.txt | head -45
