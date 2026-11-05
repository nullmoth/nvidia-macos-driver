#!/bin/bash
# rc-ab.sh: A/B test of encoder variants with tools/encodeN in the guest (from ~/nvenc-dev): rate, luma PSNR, VMAF and
# engine time per variant. Each variant is a list of environment settings for encodeN (EncConfig knobs, ENC_* hooks).
#   rc-ab.sh SRC.nv12 W H N CODEC BITRATE 'VAR1' 'VAR2' ...      CODEC: h264 | hevc | av1; BITRATE at 60 fps (encodeN's rate)
#   e.g. rc-ab.sh ~/vt-streams/natural/crowd60.nv12 1920 1080 300 hevc 7200000 '' 'TWOPASS=1' 'TWOPASS=1 ENC_MP_NOHINT=1'
# One line per variant: "AB <codec> <variant> Mbps=<rate> psnr=<dB> vmaf=<score> ms=<encode ms per frame>"
set -uo pipefail
SRC=$1; W=$2; H=$3; N=$4; CODEC=$5; RATE=$6; shift 6
F=/usr/local/bin/ffmpeg
T=${AB_TMP:-/tmp/rcab}; mkdir -p "$T"
FS=$((W * H * 3 / 2)); SRCN=$(( $(stat -f %z "$SRC") / FS )); LOOPS=$(( (N + SRCN - 1) / SRCN - 1 ))
REF=$T/ref-$(basename "$SRC" .nv12)-${W}x${H}-$N.nv12
[ -s "$REF" ] || $F -nostdin -v error -y -f rawvideo -pix_fmt nv12 -s ${W}x${H} -stream_loop $LOOPS -i "$SRC" -frames:v $N -f rawvideo "$REF"
case $CODEC in hevc) a=(-hevc); fmt=hevc; q=(28 30) ;; av1) a=(-av1); fmt=ivf; q=(100 120) ;; *) a=(); fmt=h264; q=(26 28) ;; esac
[ -n "${AB_Q:-}" ] && q=($AB_Q)   # AB_Q="I P": the QP pair (constant QP with AB_RC=0)
R=(-f rawvideo -pix_fmt nv12 -s ${W}x${H} -r 60)
for v in "$@"; do
  out=$(env $v SRC="$SRC" RC=${AB_RC:-18} BITRATE=$RATE IDR_PERIOD=${AB_GOP:-250} ${AB_ENV:-} ./encodeN ${a[@]+"${a[@]}"} $W $H $N "${q[@]}" ${AB_REFS:-1} $T/ab.bin /dev/null 2>&1)
  ms=$(echo "$out" | grep -o "encode [0-9.]* ms avg" | tr -dc '0-9.')
  bytes=$(stat -f %z $T/ab.bin)
  $F -nostdin -v quiet -y -f $fmt -i $T/ab.bin -frames:v $N -pix_fmt nv12 -f rawvideo $T/dec.nv12
  psnr=$($F -nostdin -hide_banner "${R[@]}" -i $T/dec.nv12 "${R[@]}" -i "$REF" -lavfi "[0][1]psnr" -f null - 2>&1 | grep -o "y:[0-9.inf]*" | tail -1 | cut -c3-)
  vmaf=$($F -nostdin -hide_banner "${R[@]}" -i $T/dec.nv12 "${R[@]}" -i "$REF" -lavfi "[0][1]libvmaf=n_threads=16" -f null - 2>&1 | grep -o "VMAF score: [0-9.]*" | cut -d" " -f3)
  echo "AB $CODEC '${v:-baseline}' Mbps=$(echo "$bytes * 8 * 60 / $N / 1000000" | bc -l | cut -c1-5) psnr=$psnr vmaf=$vmaf ms=$ms $(echo "$out" | grep -c FAILED | sed 's/^0$//;s/^[1-9].*/FAILED/')"
done
