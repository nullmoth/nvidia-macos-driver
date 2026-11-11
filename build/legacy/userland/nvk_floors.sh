#!/bin/bash
# nvk_floors.sh: test-configure and compile NVK for x86_64 at each macOS floor. Test builds only, nothing is installed.
# Environment: AUDIT (the audit folder), NVK_TREE (a finished release NVK build folder: mesa/, ogkm/, host/bin),
# NVK_PATH, PKG_CONFIG_PATH, NVK_SYSROOT as for the release build. FLOORS overrides the list.
set -u
A=${AUDIT:?}; T=${NVK_TREE:?}; SYS=${NVK_SYSROOT:?}
export PATH="$T/host/bin:${NVK_PATH:?}:$PATH"
S=$A/${NVK_SRC:-nvk-src}
if [ ! -d "$S/mesa" ]; then
  mkdir -p "$S"; rsync -a --exclude /build "$T/mesa/" "$S/mesa/"; rsync -a "$T/ogkm" "$S/"
  rm -f "$S/mesa/src/nouveau/vulkan/nvkmd/nvrm/open-gpu-kernel-modules"; ln -s "$S/ogkm" "$S/mesa/src/nouveau/vulkan/nvkmd/nvrm/open-gpu-kernel-modules"
fi
for F in ${FLOORS:-15.0 14.0 13.0 12.0 11.0 10.15 10.14 10.13}; do
  O=$A/nvk${TAG:-}-$F; rm -rf "$O"; mkdir -p "$O"
  cat > "$O/cross.ini" <<EOT
[binaries]
c = ['clang', '-arch', 'x86_64', '-mmacosx-version-min=$F']
cpp = ['clang++', '-arch', 'x86_64', '-mmacosx-version-min=$F']
objc = ['clang', '-arch', 'x86_64', '-mmacosx-version-min=$F']
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
pkg_config_libdir = '$SYS/lib/pkgconfig'
bindgen_clang_arguments = ['--target=x86_64-apple-darwin', '-mmacosx-version-min=$F']
EOT
  export MACOSX_DEPLOYMENT_TARGET=$F
  t0=$(date +%s)
  meson setup "$O/build" "$S/mesa" --cross-file="$O/cross.ini" -Dbuildtype=release -Db_ndebug=true -Dvulkan-drivers=nouveau -Dgallium-drivers= -Dplatforms=macos \
    -Degl=disabled -Dglx=disabled -Dglvnd=disabled -Dllvm=disabled -Dmesa-clc=system -Dshared-glapi=disabled \
    -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dexpat=disabled -Dzstd=disabled -Dlibunwind=disabled \
    -Dlmsensors=disabled -Dvalgrind=disabled -Dxlib-lease=disabled -Dgallium-rusticl=false -Dspirv-tools=disabled \
    "-Dc_link_args=-Wl,-no_warn_duplicate_libraries" "-Dcpp_link_args=-Wl,-no_warn_duplicate_libraries" --wrap-mode=nodownload > "$O/setup.log" 2>&1
  src=$?
  brc=skipped
  if [ $src = 0 ]; then
    nice -n 10 ninja -C "$O/build" -k 0 -j6 src/nouveau/vulkan/libvulkan_nouveau.dylib > "$O/build.log" 2>&1; brc=$?
  fi
  D=$O/build/src/nouveau/vulkan/libvulkan_nouveau.dylib
  echo "floor $F setup=$src build=$brc dylib=$([ -f "$D" ] && echo yes || echo no) errors=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:|^error(\[E[0-9]+\])?:' "$O/build.log" 2>/dev/null) failed_steps=$(grep -c '^FAILED:' "$O/build.log" 2>/dev/null) warnings=$(grep -c 'warning:' "$O/build.log" 2>/dev/null) secs=$(( $(date +%s) - t0 ))" | tee -a "$A/nvk${TAG:-}-floors.summary"
  [ -f "$D" ] && { cp "$D" "$O/libvulkan_nouveau.dylib"; xcrun vtool -show-build "$D" > "$O/minos.txt" 2>&1; }
  # keep the logs and the product, drop the object files
  [ "${KEEP_BUILD:-0}" = 1 ] || rm -rf "$O/build"
done
echo ALL-DONE >> "$A/nvk${TAG:-}-floors.summary"
