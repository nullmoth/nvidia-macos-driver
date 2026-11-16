#!/bin/bash
# build_nvrm.sh <src> <ogkm tree> <out>: NVRM.kext (macOS 15, x86_64) from kexts/NVRM, linked against the RM core and the
# NVKMS archive that were built in the open-gpu-kernel-modules tree (see build_rm620.sh / build_kms620.sh).
#   <src>   the kexts/NVRM directory of the tree being built
#   <ogkm>  the built open-gpu-kernel-modules tree, e.g. .../rm620/ogkm-620.70.06
#   <out>   a new directory; an existing one is refused
# KMS_ARCHIVE=<libnvkms.a> links a different NVKMS archive (default: the one in the tree).
# Same style as accel_build.sh. Output: <out>/NVRM.kext, imports.txt and symbols.txt (before strip).
set -uo pipefail
SRC=$(cd "${1:?kexts/NVRM directory}" && pwd) || exit 2
OGKM=$(cd "${2:?built open-gpu-kernel-modules tree}" && pwd) || exit 2
OUT=${3:?output directory}
HERE=$(cd "$(dirname "$0")" && pwd)
unset NM_TAHOE   # NVRM is one binary for macOS 15 and 26; only NVAccel has a Tahoe build
[ ! -e "$OUT" ] && [ ! -L "$OUT" ] || { echo "STOP: output directory already exists: $OUT"; exit 2; }
mkdir -p "$OUT/NVRM.kext/Contents/MacOS" || exit 2
OUT=$(cd "$OUT" && pwd) || exit 2
NV="$OGKM/src/nvidia"; KMS="$OGKM/src/nvidia-modeset"
# The archives linked here must come from a tree that carries exactly this revision's patches (build_rm620.sh applies them
# and records their names): two patches once sat in the repository while the archives were built without them.
want=$(cd "$SRC/patches" && ls *.patch | LC_ALL=C sort | tr '\n' ' ')
have=$(LC_ALL=C sort "$OGKM/.nm-patches" 2>/dev/null | tr '\n' ' ')
[ "$want" = "$have" ] || { echo "STOP: $OGKM was built with patches [$have]; this revision has [$want]. Run build_rm620.sh and build_kms620.sh in a new work dir"; exit 2; }
[ -f "$NV/_out/Darwin_x86_64/libnvkernel.a" ] || { echo "STOP: $NV/_out/Darwin_x86_64/libnvkernel.a is not built"; exit 1; }
KMSLIB=${KMS_ARCHIVE:-$KMS/_out/Darwin_x86_64/libnvkms.a}
[ -f "$KMSLIB" ] || { echo "STOP: no NVKMS archive at $KMSLIB"; exit 1; }
# Match actual patched source bytes and archive digests, rather than filenames.
python3 - "$SRC" "$OGKM" "$KMSLIB" <<'PY' || exit 2
import hashlib, json, pathlib, sys
src, tree, kms = map(pathlib.Path, sys.argv[1:])
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
identity = json.loads((tree / '.nm-source.json').read_text())
assert identity['nvidia_commit'] == 'e6a2e01579fbbd1ca2ed43fef2ee2589e9bd8bd9'
assert identity['patches'] == {p.name: sha(p) for p in sorted((src / 'patches').glob('*.patch'))}, 'patch bytes changed'
assert all(sha(tree / name) == digest for name, digest in identity['files'].items()), 'NVIDIA source changed'
for archive, marker in [(tree / 'src/nvidia/_out/Darwin_x86_64/libnvkernel.a', '.nm-rm-archive.sha256'),
                        (kms, '.nm-kms-archive.sha256')]:
    assert sha(archive) == (tree / marker).read_text().split()[0], 'archive differs from the recorded build'
PY
# Every C++ source the kext needs, listed; a new .cpp that is not named here stops the build instead of being left out.
CXX_SOURCES=( NVRM.cpp nv_xnu_power.cpp nv_xnu_stubs.cpp nvkms_shaders.cpp nvrm_fbinfo.cpp nvrm_gpuva.cpp
              nvrm_power.cpp nvrm_surfshare.cpp os-nvkms-xnu.cpp os-xnu.cpp os-xnu2.cpp )
for f in "$SRC"/*.cpp; do
  b=$(basename "$f"); found=0
  for s in "${CXX_SOURCES[@]}"; do [ "$s" = "$b" ] && found=1; done
  [ $found = 1 ] || { echo "STOP: $b is in $SRC but not in CXX_SOURCES of build_nvrm.sh"; exit 1; }
done
for s in "${CXX_SOURCES[@]}"; do [ -f "$SRC/$s" ] || { echo "STOP: $s is listed but missing from $SRC"; exit 1; }; done
SDK=$(xcrun --show-sdk-path) || exit 2
KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"
RE=${RE:-$SRC/accel/re}   # the reconstructed IOGraphicsAccelerator2 / IOAccelerator headers ship in the repo
RMINC=( -I"$NV/arch/nvalloc/unix/include" -I"$NV/arch/nvalloc/common/inc" -I"$NV/arch/nvalloc/common/inc/gsp"
  -I"$OGKM/src/common/sdk/nvidia/inc" -I"$OGKM/src/common/sdk/nvidia/inc/hw" -I"$OGKM/src/common/inc"
  -I"$OGKM/src/common/shared/inc" -I"$NV/inc" -I"$NV/inc/os" -I"$NV/inc/kernel" -I"$NV/kernel/inc" -I"$NV/interface"
  -I"$OGKM/src/common/nvlink/interface" -I"$OGKM/src/common/inc/swref" -I"$OGKM/src/common/inc/swref/published"
  -I"$NV/inc/libraries" -I"$NV/src/libraries" -I"$NV/generated" -I"$NV/src/mm/uvm/interface"
  -I"$OGKM/src/common/uproc/os/libos-v2.0.0/include" -I"$OGKM/src/common/uproc/os/common/include"
  -I"$OGKM/src/common/inc/displayport" -I"$OGKM/src/common/nvlink/inband/interface" )
KMSINC=( -I"$KMS/os-interface/include" -I"$KMS/kapi/interface" -I"$KMS/kapi/include" -I"$OGKM/src/common/unix/nvidia-push/interface" -I"$KMS/interface" -I"$KMS/include"
         -I"$OGKM/src/common/unix/common/inc" -I"$OGKM/src/common/modeset" -I"$OGKM/src/common/unix/common/utils/interface" )
RMDEFS=(); while read -r d; do RMDEFS+=("$d"); done < <(head -1 "$NV/_out/Darwin_x86_64/compile_cmds.sh" | tr ' ' '\n' | grep -E '^-D' | sed -e 's/"//g' | grep -v -E '^-D(NVRM|_LANGUAGE_C)$')
# -g0 and the prefix maps keep the object free of build paths, so the same sources give the same binary on any build host
CFLAGS=( -arch x86_64 -fapple-kext -mkernel -nostdinc -I"$KHDR" -I"$SRC" "${RMINC[@]}" "${KMSINC[@]}"
  -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT "${RMDEFS[@]}"
  -mmacosx-version-min=15.0 -g0 -ffile-prefix-map="$SRC"=driver -ffile-prefix-map="$OGKM"=nvidia -ffile-prefix-map="$HOME"=/build -fdebug-prefix-map="$HOME"=/build
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common
  -Wall -Wno-unused-parameter -Wno-inconsistent-missing-override -Wno-unused-function -O2 )
OBJS=()
for s in "${CXX_SOURCES[@]}"; do
  o="$OUT/${s%.cpp}.o"
  xcrun clang++ "${CFLAGS[@]}" -c "$SRC/$s" -o "$o" 2>"$o.err" || { echo "STOP: $s did not compile"; grep error "$o.err" | head; exit 1; }
  OBJS+=("$o")
done
xcrun clang "${CFLAGS[@]}" -x c -std=gnu11 -c "$SRC/nvrm_kapi_fields.c" -o "$OUT/nvrm_kapi_fields.o" 2>"$OUT/kapi.err" || { echo "STOP: nvrm_kapi_fields.c did not compile"; head "$OUT/kapi.err"; exit 1; }
OBJS+=("$OUT/nvrm_kapi_fields.o")
# nvrm_pageoff.c includes RM-internal headers: compiled with the RM's own command line for osapi.c (rmcc.py)
OGKM="$OGKM" python3 -B "$HERE/../kexts/NVRM/rmcc.py" "$SRC/nvrm_pageoff.c" "$OUT/nvrm_pageoff.o" 2>"$OUT/pageoff.err" || { echo "STOP: nvrm_pageoff.c did not compile"; tail "$OUT/pageoff.err"; exit 1; }
OBJS+=("$OUT/nvrm_pageoff.o")
# The notebook TGP control also needs RM-internal GPU headers and matching definitions.
OGKM="$OGKM" python3 -B "$HERE/../kexts/NVRM/rmcc.py" "$SRC/nvrm_pmgr.c" "$OUT/nvrm_pmgr.o" 2>"$OUT/pmgr.err" || { echo "STOP: nvrm_pmgr.c did not compile"; tail "$OUT/pmgr.err"; exit 1; }
OBJS+=("$OUT/nvrm_pmgr.o")
xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "${OBJS[@]}" \
  "$NV/_out/Darwin_x86_64/libnvkernel.a" "$KMSLIB" -o "$OUT/NVRM.kext/Contents/MacOS/NVRM" 2>"$OUT/link.err" || { echo "STOP: link failed"; grep -v "built for newer" "$OUT/link.err" | head; exit 1; }
cp "$SRC/Info.plist" "$OUT/NVRM.kext/Contents/Info.plist"
xcrun nm -u "$OUT/NVRM.kext/Contents/MacOS/NVRM" > "$OUT/imports.txt"
xcrun nm "$OUT/NVRM.kext/Contents/MacOS/NVRM" > "$OUT/symbols.txt"
xcrun strip -S "$OUT/NVRM.kext/Contents/MacOS/NVRM"
echo "NVRM $(shasum -a 256 "$OUT/NVRM.kext/Contents/MacOS/NVRM" | cut -c1-16) $(stat -f %z "$OUT/NVRM.kext/Contents/MacOS/NVRM") B uuid $(xcrun otool -l "$OUT/NVRM.kext/Contents/MacOS/NVRM" | awk '/LC_UUID/{getline; getline; print $2; exit}') from $(git -C "$SRC" rev-parse --short HEAD 2>/dev/null || echo "$SRC")"
