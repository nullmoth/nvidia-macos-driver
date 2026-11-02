#!/bin/bash
# bench-enc.sh: encoder benchmark in the guest (run from ~/nvenc-dev after ./build.sh nvencvt vtencfile).
#   bench-enc.sh SRC.nv12 W H FPS FRAMES BITRATE [encoder ...]
# SRC is raw NV12 (looped to FRAMES); every encoder gets the same frames and the same target bitrate. Prints one line per
# encoder (each encode runs twice, the faster time counts: single runs in the VM sometimes stall for seconds): "RESULT name WxH fps=<encode fps> kbps=<rate> psnr=<luma dB> vmaf=<score>" (VMAF only with BENCH_VMAF=1).
# Encoders (default: all):
#   ours-h264-rt ours-h264 ours-hevc-rt ours-hevc ours-hevc10 ours-av1-rt ours-av1 ours-av1-10   (libnvencvt through
#     VideoToolbox: ffmpeg h264/hevc_videotoolbox with the library injected = what `nvenc-run ffmpeg` does; AV1 through
#     tools/vtencfile, ffmpeg has no AV1 VideoToolbox encoder). "-rt" = RealTime, P only, fast controls (Parsec / OBS
#     streaming); without = not real time: 2 B pictures + quality controls (recordings, exports)
#   apple-h264 apple-hevc      Apple's VideoToolbox encoders (software on this Mac; they run in VTEncoderXPCService)
#   x264-veryfast x264-medium x265-fast x265-medium aom-rt   ffmpeg's software encoders, 16 threads
set -uo pipefail
SRC=$1; W=$2; H=$3; FPS=$4; N=$5; RATE=$6; shift 6
ENCS=${*:-ours-h264-rt ours-h264 ours-hevc-rt ours-hevc ours-hevc10 ours-av1-rt ours-av1 ours-av1-10 apple-h264 apple-hevc x264-veryfast x264-medium x265-fast x265-medium aom-rt}
F=/usr/local/bin/ffmpeg
LIB=$PWD/libnvencvt.dylib
T=${BENCH_TMP:-/tmp/bench}; mkdir -p "$T"
FS=$((W * H * 3 / 2)); SRCN=$(( $(stat -f %z "$SRC") / FS )); LOOPS=$(( (N + SRCN - 1) / SRCN - 1 ))
IN=(-f rawvideo -pix_fmt nv12 -s ${W}x${H} -r $FPS -stream_loop $LOOPS -i "$SRC" -frames:v $N)
# the reference: SRC looped to N frames (raw)
REF=$T/ref-$(basename "$SRC" .nv12)-${W}x${H}-$N.nv12
[ -s "$REF" ] || $F -nostdin -v error -y "${IN[@]}" -f rawvideo "$REF"
now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
metrics() {   # $1 = encoded file: decode to NV12 (8-bit), luma PSNR (and VMAF) against REF
  $F -nostdin -v error -y -i "$1" -frames:v $N -pix_fmt nv12 -f rawvideo "$T/dec.nv12"
  local R=(-f rawvideo -pix_fmt nv12 -s ${W}x${H} -r $FPS)
  PSNR=$($F -nostdin -hide_banner "${R[@]}" -i "$T/dec.nv12" "${R[@]}" -i "$REF" -lavfi "[0][1]psnr" -f null - 2>&1 | grep -o "y:[0-9.inf]*" | tail -1 | cut -c3-)
  VMAF=-
  [ -n "${BENCH_VMAF:-}" ] && VMAF=$($F -nostdin -hide_banner "${R[@]}" -i "$T/dec.nv12" "${R[@]}" -i "$REF" -lavfi "[0][1]libvmaf=n_threads=16" -f null - 2>&1 | grep -o "VMAF score: [0-9.]*" | cut -d" " -f3)
}
for e in $ENCS; do
  out=$T/$e.mp4; inj=(); args=()
  case $e in
    ours-h264-rt) inj=(env DYLD_INSERT_LIBRARIES=$LIB); args=(-c:v h264_videotoolbox -realtime 1 -bf 0) ;;
    ours-h264)    inj=(env DYLD_INSERT_LIBRARIES=$LIB); args=(-c:v h264_videotoolbox -bf 2) ;;
    ours-hevc-rt) inj=(env DYLD_INSERT_LIBRARIES=$LIB); args=(-c:v hevc_videotoolbox -realtime 1 -bf 0 -tag:v hvc1) ;;
    ours-hevc)    inj=(env DYLD_INSERT_LIBRARIES=$LIB); args=(-c:v hevc_videotoolbox -bf 2 -tag:v hvc1) ;;
    ours-hevc10)  inj=(env DYLD_INSERT_LIBRARIES=$LIB); args=(-pix_fmt p010le -c:v hevc_videotoolbox -profile:v main10 -bf 2 -tag:v hvc1) ;;
    apple-h264)   args=(-c:v h264_videotoolbox -allow_sw 1 -bf 2) ;;
    apple-hevc)   args=(-c:v hevc_videotoolbox -allow_sw 1 -bf 2 -tag:v hvc1) ;;
    x264-veryfast) args=(-c:v libx264 -preset veryfast -threads 16) ;;
    x264-medium)  args=(-c:v libx264 -preset medium -threads 16) ;;
    x265-fast)    args=(-c:v libx265 -preset fast -x265-params log-level=error:pools=16) ;;
    x265-medium)  args=(-c:v libx265 -preset medium -x265-params log-level=error:pools=16) ;;
    aom-rt)       args=(-c:v libaom-av1 -usage realtime -cpu-used 8 -row-mt 1 -threads 16) ;;
    ours-av1*)    out=$T/$e.ivf ;;
    *) echo "unknown encoder $e"; continue ;;
  esac
  if [[ $e == ours-av1* ]]; then   # vtencfile reads the raw file itself (VT_SRC, looped) and writes IVF
    ve=(env DYLD_INSERT_LIBRARIES=$LIB VT_SRC="$SRC" VT_BITRATE=$RATE VT_FPS=$FPS)
    [ $e = ours-av1-rt ] && ve+=(VT_REALTIME=1) || ve+=(VT_REORDER=1)
    [ $e = ours-av1-10 ] && ve+=(VT_10BIT=1)
    best=999999
    for rep in 1 2; do t0=$(now); "${ve[@]}" ./vtencfile av01 $W $H $N "$out" /dev/null > "$T/log" 2>&1; t1=$(now); d=$(echo "$t1 - $t0" | bc -l); [ $(echo "$d < $best" | bc) = 1 ] && best=$d; done
    grep -q "RESULT $N of $N" "$T/log" || { echo "RESULT $e ${W}x${H} FAILED: $(tail -1 "$T/log")"; continue; }
  else
    best=999999
    for rep in 1 2; do t0=$(now); ${inj[@]+"${inj[@]}"} $F -nostdin -v error -y "${IN[@]}" "${args[@]}" -b:v $RATE -g 250 "$out" 2> "$T/log"; t1=$(now); d=$(echo "$t1 - $t0" | bc -l); [ $(echo "$d < $best" | bc) = 1 ] && best=$d; done
    [ -s "$out" ] || { echo "RESULT $e ${W}x${H} FAILED: $(tail -1 "$T/log")"; continue; }
  fi
  sz=$(stat -f %z "$out")
  metrics "$out"
  echo "RESULT $e ${W}x${H} fps=$(echo "$N / $best" | bc -l | cut -c1-6) kbps=$(( sz * 8 * FPS / N / 1000 )) psnr=$PSNR vmaf=$VMAF"
done
