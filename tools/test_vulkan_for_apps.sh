#!/bin/bash
# Host test for package/vulkan-for-apps.sh in a scratch prefix. Every arm is a sentence; exit 0 only when all hold.
S=$(cd "$(dirname "$0")/.." && pwd)/package/vulkan-for-apps.sh
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
export NULLMOTH_VK_PREFIX=$T/prefix NULLMOTH_VK_LIBDIR=$T/lib
fail=0
arm() { if "${@:2}"; then echo "ok    $1"; else echo "FAIL  $1"; fail=1; fi; }
runs() { bash "$S" "$@" >/dev/null 2>&1; }
refuses() { ! bash "$S" "$@" >/dev/null 2>&1; }
M=$T/prefix/share/vulkan/icd.d/nullmoth_nvk_icd.json

arm "install without the driver's NVK library stops and writes nothing" bash -c "! bash '$S' --install >/dev/null 2>&1 && [ ! -e '$T/prefix' ]"
mkdir -p "$T/lib"; : > "$T/lib/libvulkan_nouveau.dylib"
arm "install with the library present succeeds" runs --install
arm "the manifest is valid JSON naming the NVK library" python3 -c "import json; d=json.load(open('$M')); assert d['ICD']['library_path']=='$T/lib/libvulkan_nouveau.dylib' and d['file_format_version']=='1.0.0'"
arm "no temporary file is left in the loader folder" bash -c "[ \$(ls -A '$T/prefix/share/vulkan/icd.d' | wc -l) -eq 1 ]"
arm "installing twice is harmless" runs --install
arm "status reports it installed" bash -c "bash '$S' --status | grep -q 'manifest: installed'"
printf 'int nullmoth_test_symbol;\n' > "$T/l.c"; cc -arch x86_64 -dynamiclib "$T/l.c" -o "$T/libvulkan.1.dylib"
printf 'int arm64only;\n' > "$T/a.c"; cc -arch arm64 -dynamiclib "$T/a.c" -o "$T/arm.dylib"
arm "a missing loader file is refused and nothing is copied" bash -c "! bash '$S' --install --with-loader '$T/missing.dylib' >/dev/null 2>&1 && [ ! -e '$T/prefix/lib' ]"
arm "an arm64-only loader is refused" bash -c "! bash '$S' --install --with-loader '$T/arm.dylib' >/dev/null 2>&1 && [ ! -e '$T/prefix/lib' ]"
arm "an x86_64 loader is installed with the manifest" bash -c "bash '$S' --install --with-loader '$T/libvulkan.1.dylib' >/dev/null 2>&1 && [ -x '$T/prefix/lib/libvulkan.1.dylib' ] && [ -f '$M' ]"
runs --remove
arm "remove deletes the manifest" bash -c "[ ! -e '$M' ]"
arm "removing when absent is harmless" runs --remove
arm "an unknown switch is refused" refuses --bogus
if [ $fail = 0 ]; then echo "PASS all arms"; fi
exit $fail
