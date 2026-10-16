#!/bin/bash
# build.sh <out dir>: NMXAudio.kext - FreeBSD's HD Audio driver (hdac, hdacc, hdaa, quirks) and the port's sound core
# and audio engine, on the FreeBSD-on-xnu layer (srcobj, xnuobj without NMXGPU's and NMXNPU's kext glue). No LinuxKPI.
set -euo pipefail
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
A=$X/audio
F=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi/freebsd-src/sys/dev/sound
OUT=$1
cd "$A"
mkdir -p gen obj
for m in "$F/pcm/channel_if.m" "$F/pcm/mixer_if.m" "$F/pci/hda/hdac_if.m"; do
    /opt/homebrew/bin/python3 mkif.py "$m" "gen/$(basename "$m" .m).h"
done
rm -f obj/*.o
for s in "$F/pci/hda/hdac.c" "$F/pci/hda/hdacc.c" "$F/pci/hda/hdaa.c" "$F/pci/hda/hdaa_patches.c" nmx_snd_compat.c nmx_pcm.c nmx_audio_stubs.c; do
    ./cc-snd.sh "$s" > "log-$(basename "$s" .c).txt" 2>&1 || { grep -A3 error: "log-$(basename "$s" .c).txt" | head -20; exit 1; }
done
# the IOKit side, as ccxx.sh compiles NMXGPU's (Kernel.framework headers only, never the FreeBSD tree) plus the sound
# core's C interface (include/nmx_pcm.h, plain types)
SDK=$(xcrun --sdk macosx --show-sdk-path); KF=$SDK/System/Library/Frameworks/Kernel.framework/Headers
cp -p "$X"/bsdcompat/include/nmx_port.h "$X"/bsdcompat/include/nmx_xnu.h "$X"/bsdcompat/include/nmxxnu/
xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -ffile-prefix-map=$NULLMOTH_BUILD_ROOT/build/gpu-src=/src -mkernel -fapple-kext \
  -nostdinc -isysroot "$SDK" -I"$KF" -I"$X"/bsdcompat/include/nmxxnu -I"$A"/xnuinc -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE \
  -DAPPLE -DNeXT -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -O2 -g0 -Wall -Wno-unused-parameter \
  -Wno-deprecated-declarations -Wno-inconsistent-missing-override -c nmx_audio_kext.cpp -o obj/nmx_audio_kext.o \
  > log-kext.txt 2>&1 || { grep -A3 error: log-kext.txt | head -30; exit 1; }
XNU=$(ls "$X"/xnuobj/*.o | grep -vE "/(nmx_kext|nmx_fb|nmx_npu_kext)\.o$")
K="$OUT/NMXAudio.kext/Contents"
rm -rf "$OUT/NMXAudio.kext"; mkdir -p "$K/MacOS"
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib -Xlinker -kext \
  obj/*.o "$X"/srcobj/*.o $XNU -lkmodc++ -lkmod -lcc_kext -o "$K/MacOS/NMXAudio" 2> link.log || {
    grep -E "error|Undefined|duplicate|referenced from" link.log | head -60; exit 1; }
cp Info.plist "$K/Info.plist"
echo "NMXAudio $(shasum -a 256 "$K/MacOS/NMXAudio" | cut -c1-8) $(stat -f %z "$K/MacOS/NMXAudio") B"
