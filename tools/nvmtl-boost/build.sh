#!/bin/bash
# build.sh <patched Mesa tree> <out>: builds nvmtl-boost (x86_64, macOS 15) against NVK's RM layer (nvRmApi.c, nvrm_xnu.c)
# and NVIDIA's open-gpu-kernel-modules headers that tree links to (src/nouveau/vulkan/nvkmd/nvrm/open-gpu-kernel-modules).
set -euo pipefail
T=${1:?patched Mesa tree}; OUT=${2:?output file}
N=$T/src/nouveau/vulkan/nvkmd/nvrm; O=$N/open-gpu-kernel-modules; H=$(cd "$(dirname "$0")" && pwd)
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -O1 -Wall -w -o "$OUT" "$H/nvmtl_boost.c" "$N/nvRmApi.c" "$N/nvrm_xnu.c" \
  -I"$H" -I"$N" -I"$O/src/nvidia/arch/nvalloc/common/inc" -I"$O/src/nvidia/arch/nvalloc/unix/include" -I"$O/src/common/sdk/nvidia/inc" \
  -I"$O/src/nvidia/inc/kernel" -framework IOKit -framework CoreFoundation
echo "built $OUT"
