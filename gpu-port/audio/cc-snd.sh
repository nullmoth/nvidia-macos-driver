#!/bin/bash
# cc-snd.sh <source.c> [extra flags]: compiles one FreeBSD sound source (or the port's own C) for the macOS kernel
# (x86_64) on the FreeBSD-on-xnu layer. No LinuxKPI: the HD Audio driver is plain FreeBSD. The port's include dir
# (our sound.h, nmx_kobj.h, generated *_if.h) comes first, then bsdcompat, then FreeBSD's own sys/ for the rest
# (sys/soundcard.h, dev/sound/pcm/matrix.h, the hda headers) after the SDK.
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
F=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi/freebsd-src/sys
A=$X/audio
SDK=$(xcrun --sdk macosx --show-sdk-path)
KF=$SDK/System/Library/Frameworks/Kernel.framework/Headers
src=$1; shift
mkdir -p "$A/obj"
exec xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -ffile-prefix-map=$NULLMOTH_BUILD_ROOT/build/gpu-src=/src -mkernel -fapple-kext -nostdinc -std=gnu11 \
  -fno-builtin -fno-common -fno-strict-aliasing -fms-extensions -O2 -g0 \
  -Wno-pointer-sign -Wno-unused-parameter -Wno-microsoft-anon-tag -ferror-limit=60 \
  -D_KERNEL -DKERNEL -DKERNEL_PRIVATE -DNMXKPI=1 \
  -include $X/bsdcompat/include/nmxkpi_prelude.h \
  -include $A/include/nmx_snd_compat.h \
  -I$A/include -I$A/xnuinc -I$A/gen -I$X/bsdcompat/include -I$KF -idirafter $F \
  "$@" -c "$src" -o ${NMX_OBJ:-$A/obj/$(basename "${src%.c}").o}
