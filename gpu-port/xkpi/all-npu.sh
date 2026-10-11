#!/bin/bash
# all-npu.sh [N]: compiles Intel's NPU driver (ivpu) and the DRM accel core from Linux 6.14 into obj-npu/ (GPL-2.0:
# they link into NMXNPU.kext only) and ranks the remaining errors.
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
V=$NULLMOTH_BUILD_ROOT/build/gpu-src/linux-6.14
cd "$X" || exit 1
mkdir -p obj-npu logs-npu override/npu && rm -f logs-npu/*.log logs-npu/*.failed
/opt/homebrew/bin/python3 fork_npu.py "$V" npu-src $NULLMOTH_BUILD_ROOT/build/gpu-src/linux || exit 1
# ivpu_debugfs.c and ivpu_coredump.c: debugfs and devcoredump do not exist on macOS (their headers stub them)
ls npu-src/ivpu/*.c npu-src/drm_accel.c npu-src/drm_gem_shmem_helper.c | grep -v "ivpu_debugfs.c\|ivpu_coredump.c" > srcs-npu.txt
# AMD XDNA: built separately per file with NMX_XDNA (its own include paths); listed after ivpu
# as its Makefile builds it without CONFIG_PCI_IOV and CONFIG_DEBUG_FS
ls npu-src/amdxdna/*.c | grep -vE "/(amdxdna_debugfs|aie4_sriov)\.c$" >> srcs-npu.txt
for f in "$X"/override/npu/*.c; do [ -f "$f" ] && echo "$f" >> srcs-npu.txt; done
n=$(wc -l < srcs-npu.txt | tr -d ' ')
one() { b=$(basename "$1" .c); ./cc-npu.sh "$1" > logs-npu/$b.log 2>&1 || touch logs-npu/$b.failed; }
export -f one
xargs -P 10 -I{} bash -c 'one "$@"' _ {} < srcs-npu.txt
bad=$(find logs-npu -name "*.failed" | wc -l | tr -d ' ')
echo "sources: $n  compiled: $((n - bad))  failed: $bad"
for f in $(find logs-npu -name "*.failed"); do cat "${f%.failed}.log"; done | grep -h "error:" | sed -E 's|^[^ ]*/([^/]+:[0-9]+):[0-9]+: error: |\1 |' | sed -E 's/^[^ ]+ //' \
    | sort | uniq -c | sort -rn | head -"${1:-40}"
