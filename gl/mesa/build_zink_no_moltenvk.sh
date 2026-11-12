#!/bin/bash
# build_zink_no_moltenvk.sh <patched mesa tree> <deployment target> <out dir>
# Proof for patch_zink_no_moltenvk.py: zink configures with NO moltenvk-dir and libzink.a compiles and archives.
# Needs the same tool path as the Vulkan driver build (meson, ninja, a current bison, mesa_clc) on PATH.
set -uo pipefail
S=${1:?}; T=${2:?}; O=${3:?}; mkdir -p "$O"; B=$S/build-$T-nomvk; rm -rf "$B"
cat > "$O/cross.ini" <<EOT
[binaries]
c = ['clang', '-arch', 'x86_64', '-mmacosx-version-min=$T']
cpp = ['clang++', '-arch', 'x86_64', '-mmacosx-version-min=$T']
objc = ['clang', '-arch', 'x86_64', '-mmacosx-version-min=$T']
rust = ['rustc', '--target', 'x86_64-apple-darwin']
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
EOT
meson setup "$B" "$S" --cross-file="$O/cross.ini" -Dbuildtype=release -Db_ndebug=true -Dvulkan-drivers= -Dgallium-drivers=zink \
  -Dplatforms=macos -Dopengl=true -Degl=disabled -Dglx=disabled -Dgbm=disabled -Dgles1=disabled -Dgles2=enabled -Dglvnd=disabled \
  -Dllvm=disabled -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dexpat=disabled -Dzstd=disabled -Dlibunwind=disabled \
  -Dlmsensors=disabled -Dvalgrind=disabled -Dxlib-lease=disabled -Dgallium-rusticl=false -Dspirv-tools=disabled \
  -Dgallium-va=disabled -Dvideo-codecs= > "$O/setup.log" 2>&1
rc=$?; echo "setup without moltenvk-dir: rc=$rc $(grep -m1 'ERROR' "$O/setup.log" | cut -c1-140)"; [ $rc = 0 ] || exit 1
ninja -C "$B" -k 0 -j"${JOBS:-5}" src/gallium/drivers/zink/libzink.a > "$O/build.log" 2>&1; rc=$?
echo "libzink.a: ninja rc=$rc, compiler errors $(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$O/build.log"), objects $(ar t "$B/src/gallium/drivers/zink/libzink.a" 2>/dev/null | grep -c '\.o$')"
echo "MoltenVK symbols referenced by the archive: $(nm -arch x86_64 "$B/src/gallium/drivers/zink/libzink.a" 2>/dev/null | grep -ciE 'mvk|moltenvk') (control: zink_ symbols defined: $(nm -arch x86_64 -g "$B/src/gallium/drivers/zink/libzink.a" 2>/dev/null | grep -c ' T _zink_'))"
exit $rc
