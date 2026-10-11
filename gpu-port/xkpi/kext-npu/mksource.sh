#!/bin/bash
# kext-npu/mksource.sh <out.tar.gz>: NMXNPU's complete corresponding source (GPL-2.0). Every source compiled into
# NMXNPU.kext is compiled once more with -MD into a scratch object, and every file the compiler read (sources and
# headers; Apple's SDK headers excepted, as system headers) is copied in, at its path under the build root: xkpi/ for
# the NullMoth layer and scripts, upstream/ for the Linux, drm-kmod, LinuxKPI and ACPICA files. Plus the build scripts
# and NOTICE-GPL.txt. Run after all-npucore.sh, all-npu.sh and kext-npu/build.sh.
set -euo pipefail
G=$NULLMOTH_BUILD_ROOT/build/gpu-src
X=$G/xkpi
K=$G/kpi
L=$K/freebsd-src/sys/compat/linuxkpi/common/src
OUT=$1
T=$(mktemp -d /tmp/nmxnpu-src.XXXXXX)
trap 'rm -rf "$T"' EXIT
R=$T/nmxnpu-source; DEP=$T/deps; mkdir -p "$R" "$DEP"
cd "$X"
dep() {  # dep <script> <source> [flags...]: compile once more, recording what was read
    local s=$1 src=$2; shift 2
    case "$src" in /*) ;; *) src=$X/$src ;; esac     # absolute, so the .d file names it under the build root
    NMX_OBJ=$T/scratch.o ./$s "$src" "$@" -MD -MF "$DEP/$(basename "$src").d" > "$DEP/$(basename "$src").log" 2>&1 \
        || { echo "STOP: $src did not compile for the source list"; tail -5 "$DEP/$(basename "$src").log"; exit 1; }
}
while read -r f; do dep cc-npucore.sh "$f"; done < srcs-npucore.txt
while read -r f; do dep cc-npu.sh "$f"; done < srcs-npu.txt
# the runtime as kext-npu/build.sh links it (rt.sh's list, minus the GPU-only files)
SKIP=" linux_80211 linux_80211_macops linuxkpi_80211_pm linux_netdev linux_usb linux_i2c linux_i2cbb linux_cmdline linux_eventfd nmx_fb_bridge nmx_intel_stolen nmx_drm_os "
for f in "$L"/*.c override/src/*.c; do
    b=$(basename "$f" .c)
    case "$SKIP" in *" $b "*) continue ;; esac
    [ "$f" != "override/src/$b.c" ] && [ -f "override/src/$b.c" ] && continue
    dep cclk.sh "$f"
done
dep cclk.sh override/src/nmx_drm_os.c -DNMX_NO_TTM
dep cclk.sh "$K/freebsd-src/sys/contrib/ck/src/ck_epoch.c"
for f in bsdcompat/src/*.c; do dep ccsrc.sh "$f"; done
# the IOKit glue (C++, ccxx.sh takes no extra flags, so no dependency file): its sources and the two boundary headers
# it includes, copied as they are (everything else it reads is Apple's SDK)
mkdir -p "$R/xkpi/xnu" "$R/xkpi/bsdcompat/include/nmxxnu"
for f in xnu/*.cpp; do case "$(basename "$f")" in nmx_kext.cpp|nmx_fb.cpp) continue ;; esac; cp -p "$f" "$R/xkpi/xnu/"; done
cp -p bsdcompat/include/nmxxnu/*.h "$R/xkpi/bsdcompat/include/nmxxnu/"
# every file read under the build root (make-style .d files: "target: dep dep \")
cat "$DEP"/*.d | tr ' \\' '\n\n' | grep "^$G/" | sort -u > "$T/files.txt"
while read -r f; do
    rel=${f#$G/}
    case "$rel" in xkpi/*) dst="$R/$rel" ;; *) dst="$R/upstream/$rel" ;; esac
    mkdir -p "$(dirname "$dst")"; cp -pL "$f" "$dst"
done < "$T/files.txt"
# the build scripts and the notice
mkdir -p "$R/xkpi/kext-npu"
cp -p "$X"/cc-npu.sh "$X"/cc-npucore.sh "$X"/cc.sh "$X"/cclk.sh "$X"/ccsrc.sh "$X"/ccxx.sh "$X"/all-npu.sh "$X"/all-npucore.sh \
      "$X"/rt.sh "$X"/srclist.py "$X"/fork_npu.py "$X"/fork_linux_*.py "$X"/amd.flags "$R/xkpi/"
cp -p "$X"/kext-npu/* "$R/xkpi/kext-npu/"
cp -p "$X/kext-npu/NOTICE-GPL.txt" "$R/NOTICE-GPL.txt"
tar -C "$T" -czf "$OUT" nmxnpu-source
echo "NMXNPU source $(shasum -a 256 "$OUT" | cut -c1-8) $(stat -f %z "$OUT") B, $(wc -l < "$T/files.txt" | tr -d ' ') files read by the compiler"
