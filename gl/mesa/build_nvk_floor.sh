#!/bin/bash
# build_nvk_floor.sh <patched mesa tree> <floor: 10.6 | 10.7 | 10.11> <out dir> <gl source dir> <10.6 SDK>
# Builds the Vulkan driver (NVK with its shader compiler) for x86_64 at one deployment floor, with the pieces that
# make it loadable on the oldest release of that floor linked INTO the driver: the C library calls that release
# lacks (context/nm_compat.c) and, at 10.6, the nine C++ library symbols (context/nm_cxx_min.cpp) with the older
# C++ library, per-thread keys instead of compiler thread-local storage (NM_NO_COMPILER_TLS), and a Rust standard
# library built without thread-local storage (NM_RUST_SYSROOT, NM_RUST_TARGET, from tools/rust_std106.sh; the
# nightly compiler that built it must be first on PATH with its RUSTUP_HOME).
set -uo pipefail
S=${1:?}; T=${2:?}; O=${3:?}; G=${4:?}; S6=${5:?}; mkdir -p "$O"; B=$S/build-nvk-$T; rm -rf "$B"
A="-arch x86_64 -mmacosx-version-min=$T"
# the two shim objects are compiled with the system compiler, as the engine build does
xcrun clang $A -std=c11 -Wall -Wextra -Werror -fvisibility=hidden -O2 -c "$G/context/nm_compat.c" -o "$O/nm_compat.o" || exit 1
# The registry's default port got a new name in a later release; code that uses the new name imports it weakly,
# and on these releases a weak data symbol that is absent is a null address read at the first use. The old name is
# the same port and exists on every release.
XC="-DkIOMainPortDefault=kIOMasterPortDefault"; XL="$O/nm_compat.o"; XLCPP="$XL"; RUST="'rustc', '--target', 'x86_64-apple-darwin'"
if [ "$T" = 10.6 ]; then
  xcrun clang++ $A -std=c++17 -Wall -Wextra -Werror -fvisibility=hidden -O2 -c "$G/context/nm_cxx_min.cpp" -o "$O/nm_cxx_min.o" || exit 1
  XC="$XC -DNM_NO_COMPILER_TLS"; XLCPP="$XL $O/nm_cxx_min.o -nostdlib++ $S6/usr/lib/libstdc++.6.dylib"
  RUST="'rustc', '--target', '${NM_RUST_TARGET:?}', '--sysroot', '${NM_RUST_SYSROOT:?}', '-Zunstable-options'"
fi
cat > "$O/cross.ini" <<EOT
[binaries]
c = ['clang', '-arch', 'x86_64', '-mmacosx-version-min=$T']
cpp = ['clang++', '-arch', 'x86_64', '-mmacosx-version-min=$T']
objc = ['clang', '-arch', 'x86_64', '-mmacosx-version-min=$T']
rust = [$RUST]
ar = 'ar'
strip = 'strip'
pkg-config = 'pkg-config'
[host_machine]
system = 'darwin'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
[properties]
pkg_config_libdir = '${NVK_SYSROOT:-/nonexistent}/lib/pkgconfig'
bindgen_clang_arguments = ['--target=x86_64-apple-darwin', '-mmacosx-version-min=$T']
EOT
meson setup "$B" "$S" --cross-file="$O/cross.ini" -Dbuildtype=release -Db_ndebug=true -Dvulkan-drivers=nouveau -Dgallium-drivers= -Dplatforms=macos \
  -Degl=disabled -Dglx=disabled -Dglvnd=disabled -Dllvm=disabled -Dmesa-clc=system -Dvulkan-layers= -Dtools= -Dbuild-tests=false \
  -Dexpat=disabled -Dzstd=disabled -Dlibunwind=disabled -Dlmsensors=disabled -Dvalgrind=disabled -Dxlib-lease=disabled \
  -Dgallium-rusticl=false -Dspirv-tools=disabled "-Dc_args=$XC" "-Dcpp_args=$XC" \
  "-Dc_link_args=$XL -Wl,-no_warn_duplicate_libraries" "-Dcpp_link_args=$XLCPP -Wl,-no_warn_duplicate_libraries" > "$O/setup.log" 2>&1
rc=$?; echo "floor $T setup: rc=$rc $(grep -m1 'ERROR' "$O/setup.log" | cut -c1-200)"; [ $rc = 0 ] || exit 1
ninja -C "$B" -k 0 -j"${JOBS:-5}" src/nouveau/vulkan/libvulkan_nouveau.dylib > "$O/build.log" 2>&1; rc=$?
D=$B/src/nouveau/vulkan/libvulkan_nouveau.dylib
echo "floor $T: ninja rc=$rc, compiler errors $(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:|^error(\[E[0-9]+\])?:' "$O/build.log"), failed steps $(grep -c '^FAILED' "$O/build.log")"
[ -f "$D" ] && { cp "$D" "$O/libvulkan_nouveau.dylib"; strip -S "$O/libvulkan_nouveau.dylib"; echo "floor $T: driver $(stat -f %z "$O/libvulkan_nouveau.dylib") bytes, links $(otool -L "$O/libvulkan_nouveau.dylib" | tail -n +2 | awk '{print $1}' | sed 's|.*/||' | xargs)"; }
exit $rc
