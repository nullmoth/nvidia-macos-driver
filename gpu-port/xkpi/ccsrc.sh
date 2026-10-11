#!/bin/bash
# cc.sh <source.c> [extra flags]: compiles one drm-kmod source for the macOS kernel (x86_64) through the
# FreeBSD-on-xnu layer (bsdcompat) and FreeBSD LinuxKPI. drm-kmod's GPL-only linuxkpi/gplv2 headers are NOT on the path.
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
K=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi
L=$K/freebsd-src/sys/compat/linuxkpi
D=$K/drm-kmod
A=$D/drivers/gpu/drm/amd
SDK=$(xcrun --sdk macosx --show-sdk-path)
KF=$SDK/System/Library/Frameworks/Kernel.framework/Headers
src=$1; shift
exec xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -ffile-prefix-map=$NULLMOTH_BUILD_ROOT/build/gpu-src=/src -mkernel -fapple-kext -nostdinc -std=gnu11 \
  -fno-builtin -fno-common -fno-strict-aliasing -fms-extensions -O2 -g0 \
  -Wno-pointer-sign -Wno-unused-parameter -Wno-microsoft-anon-tag -ferror-limit=80 \
  -D_KERNEL -DKERNEL -DKERNEL_PRIVATE -DLINUXKPI_VERSION=61400 -DNMXKPI=1 \
  -include $X/bsdcompat/include/nmxkpi_prelude.h \
  -include $L/common/include/linux/compiler_types.h -include $L/common/include/linux/kconfig.h -include $X/bsdcompat/include/nmxkpi_post.h \
  -I$X/override/include -I$X/bsdcompat/include -I$D/linuxkpi/bsd/include -I$L/common/include -I$L/dummy/include -I$X/fbroot \
  -I$D/include -I$D/include/drm -I$D/include/uapi -I$D/drivers/gpu/drm -I$D/drivers/gpu -I$D/drivers \
  -I$A/amdgpu -I$A/include -I$A/include/asic_reg -I$A/acp/include -I$A/pm/inc -I$A/pm/swsmu/inc -I$A/pm/swsmu \
  -I$A/pm/powerplay/inc -I$A/pm/legacy-dpm -I$A/amdxcp -I$A/display -I$A/display/include -I$A/display/modules/inc -I$A/display/dc -I$A/display/amdgpu_dm -I$A/amdkfd \
  $(sed "s/^/ /" $X/amd.flags | tr "\n" " ") -I$KF "$@" -c "$src" -o $X/srcobj/$(basename "${src%.c}").o
