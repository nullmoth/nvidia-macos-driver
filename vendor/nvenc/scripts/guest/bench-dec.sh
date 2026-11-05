#!/bin/bash
# bench-dec.sh: decoder benchmark in the guest (run from ~/nvenc-dev after ./build.sh nvencvt vtdecode vp8decodeN vc1decodeN).
#   bench-dec.sh SRC.nv12 W H FPS FRAMES TAG [codec ...]
# Encodes SRC (raw NV12, looped to FRAMES) once per codec with ffmpeg's software encoders at a realistic rate (cached as
# $BENCH_STREAMS/TAG-<codec>.mp4), then decodes each file with every decoder that takes it. One line per run:
#   "RESULT <codec> <decoder> WxH fps=<decode fps>"
# Codecs: h264 hevc hevc10 vp9 av1 mpeg2 mpeg4 vp8 (default: all). Decoders:
#   ours       libnvencvt through VTDecompressionSession (tools/vtdecode VTDECODE_SPEED=hw, the library injected)
#   nullmoth   the driver's own hardware decoders (H.264 / HEVC; libnvencvt's decoders left out: NVENCVT_NO_DEC=1)
#   apple-sw   Apple's software decoder through VideoToolbox (H.264 / HEVC)
#   ffmpeg-sw  ffmpeg's software decoders, 16 threads (wall time, frames discarded)
#   ours-cli   VP8: tools/vp8decodeN (no VideoToolbox consumer for VP8)
set -uo pipefail
SRC=$1; W=$2; H=$3; FPS=$4; N=$5; TAG=$6; shift 6
CODECS=${*:-h264 hevc hevc10 vp9 av1 mpeg2 mpeg4 vp8}
F=/usr/local/bin/ffmpeg
LIB=$PWD/libnvencvt.dylib
D=${BENCH_STREAMS:-$HOME/vt-streams/bench}; mkdir -p "$D"
FS=$((W * H * 3 / 2)); SRCN=$(( $(stat -f %z "$SRC") / FS )); LOOPS=$(( (N + SRCN - 1) / SRCN - 1 ))
IN=(-f rawvideo -pix_fmt nv12 -s ${W}x${H} -r $FPS -stream_loop $LOOPS -i "$SRC" -frames:v $N)
MBPS=$(( W * H / 207360 ))   # rate in Mbit/s: 10 at 1080p, 40 at 4K (8-bit H.264; the others get a bit less)
now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
for c in $CODECS; do
  ext=mp4; f=$D/$TAG-$c.$ext; [ $c = vp8 ] && f=$D/$TAG-$c.ivf
  if [ ! -s "$f" ]; then
    case $c in
      h264)   enc=(-c:v libx264 -preset medium -b:v ${MBPS}M -threads 16) ;;
      hevc)   enc=(-c:v libx265 -preset fast -b:v $((MBPS * 7 / 10))M -tag:v hvc1 -x265-params log-level=error) ;;
      hevc10) enc=(-pix_fmt yuv420p10le -c:v libx265 -preset fast -b:v $((MBPS * 7 / 10))M -tag:v hvc1 -x265-params log-level=error) ;;
      vp9)    enc=(-c:v libvpx-vp9 -deadline realtime -cpu-used 8 -row-mt 1 -threads 16 -b:v $((MBPS * 7 / 10))M) ;;
      av1)    enc=(-c:v libaom-av1 -usage realtime -cpu-used 8 -row-mt 1 -threads 16 -b:v $((MBPS * 6 / 10))M) ;;
      mpeg2)  enc=(-c:v mpeg2video -b:v $((MBPS * 3 / 2))M -maxrate $((MBPS * 3))M -bufsize $((MBPS * 2))M -bf 2) ;;
      mpeg4)  enc=(-c:v mpeg4 -b:v ${MBPS}M -bf 2 -threads 1) ;;
      vp8)    enc=(-c:v libvpx -deadline realtime -cpu-used 8 -threads 16 -b:v ${MBPS}M) ;;
      *) echo "unknown codec $c"; continue ;;
    esac
    $F -nostdin -v error -y "${IN[@]}" "${enc[@]}" -g 250 "$f" || { echo "RESULT $c encode FAILED"; continue; }
  fi
  decs="ours ffmpeg-sw"
  case $c in h264|hevc|hevc10) decs="ours nullmoth apple-sw ffmpeg-sw" ;; vp8) decs="ours-cli ffmpeg-sw" ;; esac
  pf=(); [ $c = hevc10 ] && pf=(VTDECODE_PF=x420)   # 10-bit output (as 10-bit consumers ask; 420v would add a CPU conversion)
  for d in $decs; do
    case $d in
      ours)     r=$(env ${pf[@]+"${pf[@]}"} DYLD_INSERT_LIBRARIES=$LIB VTDECODE_SUPP=1 VTDECODE_SPEED=hw ./vtdecode "$f" $N 2>&1 | grep "RESULT speed") ;;
      nullmoth) r=$(env ${pf[@]+"${pf[@]}"} DYLD_INSERT_LIBRARIES=$LIB NVENCVT_NO_DEC=1 VTDECODE_SPEED=hw ./vtdecode "$f" $N 2>&1 | grep "RESULT speed") ;;
      apple-sw) r=$(env ${pf[@]+"${pf[@]}"} VTDECODE_SPEED=sw ./vtdecode "$f" $N 2>&1 | grep "RESULT speed") ;;
      ffmpeg-sw) t0=$(now); $F -nostdin -v error -threads 16 -i "$f" -f null - ; t1=$(now)
                 r="fps=$(echo "$N / ($t1 - $t0)" | bc -l | cut -c1-6)" ;;
      ours-cli) ms=$(./vp8decodeN "$f" /dev/null 2>&1 | tail -1 | grep -o "engine [0-9.]* ms" | tr -dc '0-9.')   # engine time per frame
                r="fps=$(echo "1000 / $ms" | bc -l | cut -c1-6) (engine $ms ms/frame)" ;;
    esac
    fps=$(echo "$r" | grep -o "([0-9.]* fps)" | tr -dc '0-9.'); [ -n "$fps" ] && r="fps=$fps $(echo "$r" | grep -o "hardware [01], error -*[0-9]*")"
    echo "RESULT $c $d ${W}x${H} $r"
  done
done
