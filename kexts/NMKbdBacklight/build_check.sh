#!/bin/bash
# build_check.sh <scratch dir>: fetches Lilu and VirtualSMC (BSD-3-Clause) and Apple's kernel headers (MacKernelSDK) into the
# scratch folder, and compiles NMKbdBacklight.cpp against them for x86_64. Compile only: it proves the plugin matches the real
# SDK headers, with no warnings. Nothing is installed and nothing is loaded.
set -euo pipefail
W=${1:?scratch dir}; HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$W"; cd "$W"
for r in Lilu VirtualSMC MacKernelSDK; do [ -d $r ] || git clone -q --depth 1 https://github.com/acidanthera/$r.git; done
for r in Lilu VirtualSMC MacKernelSDK; do echo "$r $(git -C $r rev-parse --short HEAD)"; done
SDK=$(xcrun --sdk macosx --show-sdk-path)
: > kbl.err
xcrun clang++ -arch x86_64 -std=c++17 -mmacosx-version-min=10.13 -mkernel -fno-rtti -fno-exceptions -fno-builtin -nostdinc -nostdinc++ \
  -isysroot "$SDK" -Wall -Wextra -Wno-unused-parameter -Werror=return-type \
  -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT -DPRODUCT_NAME=NMKbdBacklight -DMODULE_VERSION=1.0.0 \
  -DNMKbdBacklight_kern_start=NMKbdBacklight_kern_start \
  -I MacKernelSDK/Headers -I Lilu/Lilu -I VirtualSMC -I "$HERE" \
  -c "$HERE/NMKbdBacklight.cpp" -o NMKbdBacklight.o 2>> kbl.err || { cat kbl.err; exit 1; }
xcrun clang++ -arch x86_64 -std=c++17 -mmacosx-version-min=10.13 -mkernel -fno-rtti -fno-exceptions -fno-builtin -nostdinc -nostdinc++ \
  -isysroot "$SDK" -Wall -Wextra -Wno-unused-parameter -Werror=return-type \
  -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT -DPRODUCT_NAME=NMKbdBacklight -DMODULE_VERSION=1.0.0 \
  -I MacKernelSDK/Headers -I Lilu/Lilu -I VirtualSMC -I "$HERE" \
  -c "$HERE/NMKbdBacklightHID.cpp" -o NMKbdBacklightHID.o 2>> kbl.err || { cat kbl.err; exit 1; }
echo "compiled NMKbdBacklightHID.o"
bash "$HERE/../../build/warn_ceiling.sh" nmkbdbacklight "$(grep -c ': warning:' kbl.err || true)" kbl.err
echo "compiled NMKbdBacklight.o"
