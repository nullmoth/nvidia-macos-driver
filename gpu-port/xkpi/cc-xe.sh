#!/bin/bash
# cc-xe.sh <source.c> [extra flags]: compiles one Linux 6.14 xe source (Lunar Lake, Battlemage, Panther Lake and the
# Gen12 parts xe also drives) for the macOS kernel (x86_64) through the same layer as amdgpu and i915: bsdcompat,
# FreeBSD LinuxKPI and drm-kmod's 6.14 DRM core. xe is not in drm-kmod, so it comes from the pristine 6.14 tree
# (linux-6.14, MIT except the files listed in all-xe.sh, which are replaced, never compiled). Its display code is the
# 6.14 i915 display built with xe's compat headers and -Ddrm_i915_private=xe_device, as Linux's Makefile does.
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
K=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi
L=$K/freebsd-src/sys/compat/linuxkpi
D=$K/drm-kmod
V=$NULLMOTH_BUILD_ROOT/build/gpu-src/linux-6.14
XE=$X/xe-src
SDK=$(xcrun --sdk macosx --show-sdk-path)
KF=$SDK/System/Library/Frameworks/Kernel.framework/Headers
src=$1; shift
mkdir -p $X/obj-xe
exec xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -ffile-prefix-map=$NULLMOTH_BUILD_ROOT/build/gpu-src=/src -mkernel -fapple-kext -nostdinc -std=gnu11 \
  -fno-builtin -fno-common -fno-strict-aliasing -fms-extensions -O2 -g0 \
  -Wno-pointer-sign -Wno-unused-parameter -Wno-microsoft-anon-tag -ferror-limit=80 \
  -D_KERNEL -DKERNEL -DKERNEL_PRIVATE -DLINUXKPI_VERSION=61400 -DNMXKPI=1 \
  -include $X/bsdcompat/include/nmxkpi_prelude.h \
  -include $L/common/include/linux/compiler_types.h -include $L/common/include/linux/kconfig.h -include $X/bsdcompat/include/nmxkpi_post.h -include $X/override/xe/nmx_xe_compat.h \
  -I$X/override/xe -I$X/override/include -I$X/bsdcompat/include -I$D/linuxkpi/bsd/include -I$L/common/include -I$L/dummy/include -I$X/fbroot \
  -I$D/include -I$D/include/drm -I$D/include/uapi -I$V/include/uapi -I$D/drivers/gpu/drm -I$D/drivers/gpu -I$D/drivers \
  -I$XE -I$X/obj-xe/gen -I$XE/display/ext -I$XE/compat-i915-headers -I$D/drivers/gpu/drm/i915/display -Ddrm_i915_private=xe_device \
  -DCONFIG_DRM_XE=1 -DCONFIG_DRM_XE_DISPLAY=1 -DCONFIG_DRM_XE_FORCE_PROBE='""' -DCONFIG_DRM_XE_JOB_TIMEOUT_MAX=10000 \
  -DCONFIG_DRM_XE_JOB_TIMEOUT_MIN=1 -DCONFIG_DRM_XE_TIMESLICE_MAX=10000000 -DCONFIG_DRM_XE_TIMESLICE_MIN=1 \
  -DCONFIG_DRM_XE_PREEMPT_TIMEOUT=640000 -DCONFIG_DRM_XE_PREEMPT_TIMEOUT_MAX=10000000 -DCONFIG_DRM_XE_PREEMPT_TIMEOUT_MIN=1 \
  -DCONFIG_DRM_XE_ENABLE_SCHEDTIMEOUT_LIMIT=1 \
  $(grep -v "KBUILD_MODNAME\|PARAM_PREFIX\|/drm/amd" $X/amd.flags | sed "s/^/ /" | tr "\n" " ") '-DKBUILD_MODNAME="xe"' -DLINUXKPI_PARAM_PREFIX=xe_ -DDRM_SYSCTL_PARAM_PREFIX=_xe -I$KF "$@" -c "$src" -o $X/obj-xe/$(basename "${src%.c}").o
