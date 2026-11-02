#!/bin/bash
# bench-all.sh: the whole benchmark set in the guest (from ~/nvenc-dev; sources in ~/vt-streams/natural). Writes
# RESULT lines (bench-enc.sh / bench-dec.sh formats, plus latency and 8K lines) to stdout; docs/benchmarks.md is made
# from them. Sources: Xiph crowd_run / park_joy, first 60 frames as NV12 (1080p50 and 2160p50), looped.
#   bench-all.sh [enc] [dec] [lat] [8k]     (default: all four parts)
# BENCH_ENCODERS="..." limits the encoders (bench-enc.sh names), BENCH_LAT_CODECS="avc1 hvc1" the latency codecs; e.g. a
# card without AV1 (GTX 1650): BENCH_ENCODERS="ours-h264-rt ours-h264 ours-hevc-rt ours-hevc ours-hevc10 apple-h264 apple-hevc
# x264-veryfast x264-medium x265-fast x265-medium" BENCH_LAT_CODECS="avc1 hvc1" bench-all.sh enc lat
set -uo pipefail
S=$HOME/vt-streams/natural
PARTS=${*:-enc dec lat 8k}
F=/usr/local/bin/ffmpeg
LIB=$PWD/libnvencvt.dylib
for p in $PARTS; do case $p in
  enc)   # every encoder at one target rate per resolution; VMAF too
    for s in crowd60:1920:1080:300:6000000 park1080_60:1920:1080:300:6000000 crowd2160_60:3840:2160:300:20000000 park2160_60:3840:2160:300:20000000; do
      IFS=: read -r n w h fr rate <<< "$s"
      echo "SOURCE $n ${w}x${h} $fr frames at $rate bit/s"
      BENCH_VMAF=1 bash scripts/guest/bench-enc.sh "$S/$n.nv12" $w $h 50 $fr $rate ${BENCH_ENCODERS:-} | grep RESULT
    done ;;
  dec)
    bash scripts/guest/bench-dec.sh "$S/crowd60.nv12" 1920 1080 50 300 crowd1080 | grep RESULT
    bash scripts/guest/bench-dec.sh "$S/crowd2160_60.nv12" 3840 2160 50 300 crowd2160 | grep RESULT ;;
  lat)   # real-time sessions (as Parsec / OBS streaming): time from encodeFrame to the encoded frame, libnvencvt's statistics
    for s in crowd60:1920:1080 crowd2160_60:3840:2160; do
      IFS=: read -r n w h <<< "$s"
      for c in ${BENCH_LAT_CODECS:-avc1 hvc1 av01}; do
        rm -f /tmp/lat.log
        env DYLD_INSERT_LIBRARIES=$LIB NVENCVT_STATS=/tmp/lat.log VT_REALTIME=1 VT_FPS=60 VT_BITRATE=$(( w * h * 10 )) VT_SRC="$S/$n.nv12" \
          ./vtencfile $c $w $h 600 /tmp/lat.out /dev/null > /dev/null 2>&1
        # the last full 2-second window: "entry->emit X ms (max Y)", engine time
        l=$(grep "entry->emit" /tmp/lat.log | tail -2 | head -1)
        echo "RESULT latency $c ${w}x${h} entry_emit_ms=$(echo "$l" | grep -o "entry->emit [0-9.]*" | cut -d" " -f2) max_ms=$(echo "$l" | grep -o "entry->emit [0-9.]* ms (max [0-9.]*" | grep -o "[0-9.]*$") engine_ms=$(echo "$l" | grep -o "engine [0-9.]*" | cut -d" " -f2)"
      done
    done ;;
  8k)    # 8K: the 4K source scaled to 7680x4320 (speed only); encode with ours (encodeN, the session API) and decode the result
    K=/tmp/bench8k.nv12
    [ -s $K ] || $F -nostdin -v error -y -f rawvideo -pix_fmt nv12 -s 3840x2160 -i "$S/crowd2160_60.nv12" -frames:v 30 -vf scale=7680:4320:flags=bicubic -pix_fmt nv12 -f rawvideo $K
    for c in av1 hevc h264; do
      case $c in av1) a=(-av1 7680 4320 30 100 120 3); fmt=ivf ;; hevc) a=(-hevc 7680 4320 30 30 32 3); fmt=hevc ;; h264) a=(7680 4320 30 30 32 3); fmt=h264 ;; esac
      o=$(SRC=$K ./encodeN "${a[@]}" /tmp/8k.bin /dev/null 2>&1 | grep -o "encode [0-9.]* ms avg")
      echo "RESULT 8k-encode $c 7680x4320 $o"
      $F -nostdin -v error -y -f $fmt -i /tmp/8k.bin -c copy $( [ $c = hevc ] && echo -tag:v hvc1 ) /tmp/8k.mp4
      r=$(env DYLD_INSERT_LIBRARIES=$LIB VTDECODE_SPEED=hw ./vtdecode /tmp/8k.mp4 2>&1 | grep "RESULT speed")
      echo "RESULT 8k-decode $c 7680x4320 $(echo "$r" | grep -o "([0-9.]* fps)") $(echo "$r" | grep -o "hardware [01]")"
    done ;;
esac; done
