#!/bin/bash
set -u
export PATH=$HOME/nvmtl-build/rebase263/py/bin:$HOME/nvmtl-build/rebase263/bin:$HOME/tools/glslang/bin:$HOME/bin:$HOME/.cargo/bin:$HOME/Library/Python/3.9/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
N=$HOME/nvmtl-build; S=$N/rebase263/mesa; B=$S/build263; L=$N/build.lock; O=$N/rebase263/out
[ -f "$S/VERSION" ] || { echo "STOP: no source tree at $S"; exit 2; }
[ -e "$S/.git" ] && { echo "STOP: $S has a .git (mesa would stamp git-<sha> into the binary)"; exit 2; }
until mkdir "$L" 2>/dev/null; do echo "waiting for build.lock ($(cat $L/owner 2>/dev/null))"; sleep 10; done
echo "build263 $$" > "$L/owner"; trap 'rm -rf "$L"' EXIT

if [ ! -f "$B/build.ninja" ]; then
  meson setup "$B" "$S" -Dbuildtype=debugoptimized -Dvulkan-drivers=nouveau -Dgallium-drivers= -Dplatforms= \
    -Degl=disabled -Dglx=disabled -Dglvnd=disabled -Dllvm=disabled -Dmesa-clc=system -Dshared-glapi=disabled \
    -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dexpat=disabled -Dzstd=disabled -Dlibunwind=disabled \
    -Dlmsensors=disabled -Dvalgrind=disabled -Dxlib-lease=disabled -Dgallium-rusticl=false -Dspirv-tools=disabled \
    > "$N/rebase263/setup.log" 2>&1
  rc=$?
  [ $rc = 0 ] || { echo "STOP: meson setup failed rc=$rc"; tail -30 "$N/rebase263/setup.log"; exit 3; }
fi
meson configure "$B" "-Dc_args=-ffile-prefix-map=$S=/src -ffile-prefix-map=$HOME=/build" "-Dcpp_args=-ffile-prefix-map=$S=/src -ffile-prefix-map=$HOME=/build" "-Drust_args=--remap-path-prefix=$S=/src --remap-path-prefix=$HOME=/build" > "$N/rebase263/configure.log" 2>&1 || { echo "STOP: meson configure failed"; tail -5 "$N/rebase263/configure.log"; exit 3; }
nice ninja -C "$B" -k 0 src/nouveau/vulkan/libvulkan_nouveau.dylib > "$N/rebase263/ninja.log" 2>&1
rc=$?
echo "ninja rc=$rc  errors=$(grep -c -E 'error(\[E[0-9]+\])?:' $N/rebase263/ninja.log)  FAILED=$(grep -c '^FAILED' $N/rebase263/ninja.log)"
[ $rc = 0 ] || exit 4
U=$(nm -m "$B/src/nouveau/vulkan/libvulkan_nouveau.dylib" | grep '(undefined)' | grep -v weak | grep 'dynamically looked up')
[ -z "$U" ] || { echo "STOP: unresolved strong references (dlopen fails in WindowServer):"; echo "$U"; exit 5; }
mkdir -p "$O"; D=$O/libvulkan_nouveau.dylib; cp "$B/src/nouveau/vulkan/libvulkan_nouveau.dylib" "$D"; strip -S "$D"   # debug-map stabs name every .o by absolute path
echo "built: $(shasum -a 256 $D | cut -c1-8)  uuid $(otool -l $D | awk '/uuid/{print $2; exit}')  $D"
