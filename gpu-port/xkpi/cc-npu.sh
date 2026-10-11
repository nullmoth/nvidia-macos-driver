#!/bin/bash
# cc-npu.sh <source.c> [extra flags]: compiles one Linux 6.14 NPU (accel) source - Intel's ivpu, the DRM accel core -
# for the macOS kernel through the port's layer, like cc-xe.sh. The NPU drivers are GPL-2.0, so they are built into
# their own kext (NMXNPU), never into NMXGPU.
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
K=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi
L=$K/freebsd-src/sys/compat/linuxkpi
D=$K/drm-kmod
V=$NULLMOTH_BUILD_ROOT/build/gpu-src/linux-6.14
A=$X/npu-src
SDK=$(xcrun --sdk macosx --show-sdk-path)
KF=$SDK/System/Library/Frameworks/Kernel.framework/Headers
src=$1; shift
# each driver keeps its own module name (PCI driver name and sysctl parameter prefix)
case "$src" in *amdxdna*|*nmx_xdna*) MOD=amdxdna;; *) MOD=intel_vpu;; esac
mkdir -p $X/obj-npu
exec xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -ffile-prefix-map=$NULLMOTH_BUILD_ROOT/build/gpu-src=/src -mkernel -fapple-kext -nostdinc -std=gnu11 \
  -fno-builtin -fno-common -fno-strict-aliasing -fms-extensions -O2 -g0 \
  -Wno-pointer-sign -Wno-unused-parameter -Wno-microsoft-anon-tag -ferror-limit=80 \
  -D_KERNEL -DKERNEL -DKERNEL_PRIVATE -DLINUXKPI_VERSION=61400 -DNMXKPI=1 \
  -include $X/bsdcompat/include/nmxkpi_prelude.h \
  -include $L/common/include/linux/compiler_types.h -include $L/common/include/linux/kconfig.h -include $X/bsdcompat/include/nmxkpi_post.h -include $X/override/xe/nmx_xe_compat.h -include $X/override/npu/nmx_npu_compat.h \
  -I$X/override/npu -I$A/uapi -I$X/override/include -I$X/bsdcompat/include -I$D/linuxkpi/bsd/include -I$L/common/include -I$L/dummy/include -I$X/fbroot \
  -I$D/include -I$D/include/drm -I$D/include/uapi -I$D/include/uapi/drm -I$V/include/uapi -I$D/drivers/gpu/drm -I$D/drivers/gpu -I$D/drivers \
  -I$A -I$A/ivpu -I$A/amdxdna -I$X/obj-xe/gen \
  -DCONFIG_DRM_ACCEL=1 -DCONFIG_DRM_ACCEL_IVPU=1 \
  $(grep -v "KBUILD_MODNAME\|PARAM_PREFIX\|/drm/amd" $X/amd.flags | sed "s/^/ /" | tr "\n" " ") "-DKBUILD_MODNAME=\"$MOD\"" -DLINUXKPI_PARAM_PREFIX=${MOD}_ -DDRM_SYSCTL_PARAM_PREFIX=_$MOD -I$KF "$@" -c "$src" -o ${NMX_OBJ:-$X/obj-npu/$(basename "${src%.c}").o}
