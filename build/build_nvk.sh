#!/bin/bash
# Build NVK (Mesa Vulkan driver for NVIDIA + the NAK compiler) for x86_64 macOS from pristine Mesa plus the patches in nvk/.
#
# usage: build_nvk.sh <mesa git dir> <open-gpu-kernel-modules 610.57.04 tree> <out dir> [2.9|2.8]
#
# The release argument selects the patch set (default 2.9):
#   2.9  nvk-macos, nvk-fixes-after-def16, nvk-620-kernel, nvk-recycle, nvk-slab, nvk-slabdef, nvk-hubfix,
#        nvk-perf-boost-burst, nvk-rated-tdp, nvk-gps-maxpower, then nvk/dlss/patch_dlss_cache.py
#   2.8  the set the 2.8 / 2.8.1 package was built from: the same chain without hubfix, perf-boost-burst, rated-tdp and
#        gps-maxpower, and with nvk/release-2.8/nvk-fixes-after-def16.patch (the copy-engine retry still logged to stderr)
# The NAK changes (nvk/speed267) are already part of nvk-macos.patch; the scripts there are the history of how they
# were made and are not run here.
#
# Environment (build host specifics):
#   NVK_PATH        directories put first on PATH: python with mako/pyyaml, glslangValidator, llvm-spirv, llvm-config
#                   (LLVM 22.1.x), cargo with the x86_64-apple-darwin target, bindgen, cbindgen, meson, ninja
#   NVK_SYSROOT     x86_64 prefix holding lib/pkgconfig for zlib (and any other library the build needs)
#   NVK_JOBS        parallel jobs (default 6)
#   NVK_REF_EXPORTS optional file with the exported symbols to compare against (one name per line, sorted)
#
# The Mesa git must contain commit 17ca6174 (nvk/MESA_BASE). The tree is exported with git archive, so no .git is
# present and the binary carries no git sha. Rebuilding is deterministic: the same inputs give the same LC_UUID.
set -euo pipefail

MESA_GIT=${1:?mesa git dir}; OGKM=${2:?open-gpu-kernel-modules 610.57.04 tree}; OUT=${3:?output dir}; REL=${4:-2.9}
HERE=$(cd "$(dirname "$0")/.." && pwd); NVK=$HERE/nvk
BASE=$(cat "$NVK/MESA_BASE")
JOBS=${NVK_JOBS:-6}
[ -n "${NVK_PATH:-}" ] && export PATH="$NVK_PATH:$PATH"

case $REL in
  2.9) FIXES=$NVK/nvk-fixes-after-def16.patch
       CHAIN="nvk-macos @FIXES nvk-620-kernel nvk-recycle nvk-slab nvk-slabdef nvk-hubfix nvk-perf-boost-burst nvk-rated-tdp nvk-gps-maxpower" ;;
  2.8) FIXES=$NVK/release-2.8/nvk-fixes-after-def16.patch
       CHAIN="nvk-macos @FIXES nvk-620-kernel nvk-recycle nvk-slab nvk-slabdef" ;;
  *) echo "STOP: release must be 2.9 or 2.8"; exit 2 ;;
esac

git -C "$MESA_GIT" cat-file -e "$BASE^{commit}" || { echo "STOP: $MESA_GIT has no commit $BASE"; exit 2; }
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
S=$OUT/mesa; rm -rf "$S" "$OUT/ogkm"; mkdir -p "$S"
git -C "$MESA_GIT" archive "$BASE" | tar -x -C "$S"

# Headers: NVK compiles against the 610 open kernel module headers with the 620 nvos.h change applied.
cp -R "$OGKM" "$OUT/ogkm"
patch -p1 -s --no-backup-if-mismatch -d "$OUT/ogkm" < "$NVK/ogkm-nvos-620.patch"

for p in $CHAIN; do
  f=$NVK/$p.patch; [ "$p" = "@FIXES" ] && f=$FIXES
  patch -p1 -s --no-backup-if-mismatch -d "$S" < "$f" || { echo "STOP: $f does not apply"; exit 3; }
  echo "applied $(basename "$f")"
done
# the nvrm directory comes from nvk-macos.patch
ln -s "$OUT/ogkm" "$S/src/nouveau/vulkan/nvkmd/nvrm/open-gpu-kernel-modules"
if [ "$REL" = 2.9 ]; then
  python3 "$NVK/dlss/patch_dlss_cache.py" "$S"
fi
find "$S" -name '*.orig' -o -name '*.rej' | grep . && { echo "STOP: patch left .orig/.rej files"; exit 3; } || true

# mesa_clc and vtn_bindgen2 precompile NVK's internal CL shaders to serialized NIR. They MUST be built from this same
# tree: tools from another Mesa revision give no build error and deserialize as garbage at run time.
H=$OUT/host
if [ ! -x "$H/bin/mesa_clc" ] || [ ! -x "$H/bin/vtn_bindgen2" ]; then
  meson setup "$OUT/clc-native" "$S" -Dbuildtype=release -Dprefix="$H" -Dllvm=enabled -Dmesa-clc=enabled -Dinstall-mesa-clc=true \
    -Dvulkan-drivers= -Dgallium-drivers= -Dplatforms= -Degl=disabled -Dgbm=disabled -Dgles1=disabled -Dgles2=disabled -Dglx=disabled \
    -Dopengl=false > "$OUT/clc-native.setup.log" 2>&1
  ninja -C "$OUT/clc-native" -j"$JOBS" src/compiler/clc/mesa_clc src/compiler/spirv/vtn_bindgen2 > "$OUT/clc-native.log" 2>&1
  mkdir -p "$H/bin"; cp "$OUT/clc-native/src/compiler/clc/mesa_clc" "$OUT/clc-native/src/compiler/spirv/vtn_bindgen2" "$H/bin/"
fi
export PATH="$H/bin:$PATH"

SYS=${NVK_SYSROOT:-}
X=$OUT/x86_64-nvk.ini
cat > "$X" <<EOT
[binaries]
c = ['clang', '-arch', 'x86_64', '-mmacosx-version-min=15.0']
cpp = ['clang++', '-arch', 'x86_64', '-mmacosx-version-min=15.0']
objc = ['clang', '-arch', 'x86_64', '-mmacosx-version-min=15.0']
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
bindgen_clang_arguments = ['--target=x86_64-apple-darwin', '-mmacosx-version-min=15.0']
EOT

B=$OUT/build
meson setup "$B" "$S" --cross-file="$X" -Dbuildtype=release -Db_ndebug=true -Dvulkan-drivers=nouveau -Dgallium-drivers= -Dplatforms= \
  -Degl=disabled -Dglx=disabled -Dglvnd=disabled -Dllvm=disabled -Dmesa-clc=system -Dshared-glapi=disabled \
  -Dvulkan-layers= -Dtools= -Dbuild-tests=false -Dexpat=disabled -Dzstd=disabled -Dlibunwind=disabled \
  -Dlmsensors=disabled -Dvalgrind=disabled -Dxlib-lease=disabled -Dgallium-rusticl=false -Dspirv-tools=disabled \
  "-Dc_args=-ffile-prefix-map=$S=/src -ffile-prefix-map=$HOME=/build" "-Dcpp_args=-ffile-prefix-map=$S=/src -ffile-prefix-map=$HOME=/build" \
  "-Drust_args=--remap-path-prefix=$S=/src --remap-path-prefix=$HOME=/build" > "$OUT/setup.log" 2>&1
ninja -C "$B" -j"$JOBS" src/nouveau/vulkan/libvulkan_nouveau.dylib > "$OUT/build.log" 2>&1

D=$B/src/nouveau/vulkan/libvulkan_nouveau.dylib
U=$(nm -m "$D" | grep '(undefined)' | grep -v weak | grep 'dynamically looked up' || true)
[ -z "$U" ] || { echo "STOP: unresolved strong references (dlopen fails in WindowServer):"; echo "$U"; exit 5; }
cp "$D" "$OUT/libvulkan_nouveau.dylib"; strip -S "$OUT/libvulkan_nouveau.dylib"
nm -gU "$OUT/libvulkan_nouveau.dylib" | awk '{print $3}' | sort > "$OUT/exports.txt"
UUID=$(otool -l "$OUT/libvulkan_nouveau.dylib" | awk '/uuid/{print $2; exit}')
echo "release $REL  sha256 $(shasum -a 256 "$OUT/libvulkan_nouveau.dylib" | cut -c1-8)  LC_UUID $UUID  exports $(wc -l < "$OUT/exports.txt")" | tee "$OUT/BUILD.txt"
if [ -n "${NVK_REF_EXPORTS:-}" ]; then
  diff "$NVK_REF_EXPORTS" "$OUT/exports.txt" > "$OUT/exports.diff" && echo "exports: identical to reference" || { echo "exports differ from reference:"; cat "$OUT/exports.diff"; }
fi
