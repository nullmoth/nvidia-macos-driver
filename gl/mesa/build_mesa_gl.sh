#!/bin/bash
# build_mesa_gl.sh <patched mesa tree> <deployment target> <out dir>
# Builds the static libraries the engine links: Mesa's GL API layer and state tracker, the gallium helpers, zink
# (GL on Vulkan, the product path), and softpipe with the null window system (Mesa's CPU rasteriser: the software
# fallback, and what the host tests draw with). No LLVM, no window-system front end. x86_64.
set -uo pipefail
S=${1:?}; T=${2:?}; O=${3:?}; XC=${4:-}; mkdir -p "$O"; B=$S/build-gl-$T; rm -rf "$B"
# XC: extra compiler flags for C and C++. The 10.6 build passes "-DNM_NO_COMPILER_TLS"
# after patch_mesa_no_tls.py: the compiler has no thread-local storage below 10.7.
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
meson setup "$B" "$S" --cross-file="$O/cross.ini" -Dbuildtype=release -Db_ndebug=true -Dvulkan-drivers= -Dgallium-drivers=zink,softpipe \
  -Dplatforms=macos -Dopengl=true -Degl=disabled -Dglx=disabled -Dgbm=disabled -Dgles1=disabled -Dgles2=enabled -Dglvnd=disabled \
  -Dllvm=disabled -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dexpat=disabled -Dzstd=disabled -Dlibunwind=disabled \
  -Dlmsensors=disabled -Dvalgrind=disabled -Dxlib-lease=disabled -Dgallium-rusticl=false -Dspirv-tools=disabled \
  -Dgallium-va=disabled -Dvideo-codecs= "-Dc_args=$XC" "-Dcpp_args=$XC" > "$O/setup.log" 2>&1
rc=$?; echo "setup: rc=$rc $(grep -m1 'ERROR' "$O/setup.log" | cut -c1-160)"; [ $rc = 0 ] || exit 1
TARGETS="src/mesa/libmesa.a src/mesa/glapi/shared-glapi/libglapi.a src/gallium/auxiliary/libgallium.a src/gallium/drivers/zink/libzink.a src/gallium/drivers/softpipe/libsoftpipe.a src/gallium/winsys/sw/null/libws_null.a"
# every static library target, not only the six: the helper libraries those six depend on (utilities, compiler,
# shading-language front end) are separate archives and are only built when asked for
ALL=$(ninja -C "$B" -t targets all | grep -E '^src/[^ ]*\.a: ' | cut -d: -f1 | tr '\n' ' ')
echo "static library targets: $(echo $ALL | wc -w | xargs)"
ninja -C "$B" -k 0 -j"${JOBS:-5}" $ALL > "$O/build.log" 2>&1; rc=$?
echo "ninja rc=$rc, compiler errors $(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$O/build.log"), failed steps $(grep -c '^FAILED' "$O/build.log")"
for t in $TARGETS; do [ -f "$B/$t" ] && echo "BUILT   $t" || echo "MISSING $t"; done
exit $rc
