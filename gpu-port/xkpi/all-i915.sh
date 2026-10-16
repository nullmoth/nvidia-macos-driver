#!/bin/bash
# all-i915.sh [N]: compiles every source drm-kmod's i915 Makefile builds (evaluated against amd.flags, the shared CONFIG
# set) into obj-i915/, and ranks the remaining errors. The DRM core, TTM and dma-buf come from all.sh (obj/).
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
K=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi
cd "$X" || exit 1
mkdir -p obj-i915 logs-i915 && rm -f logs-i915/*.log logs-i915/*.failed
# intel_ggtt_gmch.c: pre-Gen6 GTT through FreeBSD agp(4); override/i915/nmx_i915_gmch.c answers for it
# i915_gem_mman.c: forked by fork_i915_gem_mman.py into override/i915
# i915_module.c: forked by fork_i915_module.py into override/i915 (prints each init step's result)
EXCLUDE="intel_ggtt_gmch.c i915_gem_mman.c i915_module.c"
/opt/homebrew/bin/python3 srclist.py "$K/drm-kmod" amd.flags i915 2>/dev/null > srcs-i915.all
: > srcs-i915.txt
while read -r f; do
    case " $EXCLUDE " in
        *" $(basename "$f") "*) ;;
        *) echo "$f" >> srcs-i915.txt ;;
    esac
done < srcs-i915.all
/opt/homebrew/bin/python3 fork_i915_gem_mman.py "$K/drm-kmod" override/i915/i915_gem_mman.c >/dev/null
/opt/homebrew/bin/python3 fork_i915_module.py "$K/drm-kmod" override/i915/i915_module.c >/dev/null
for f in "$X"/override/i915/*.c; do echo "$f" >> srcs-i915.txt; done
n=$(wc -l < srcs-i915.txt | tr -d ' ')
one() { b=$(basename "$1" .c); rm -f logs-i915/$b.failed; ./cc-i915.sh "$1" > logs-i915/$b.log 2>&1 || touch logs-i915/$b.failed; }
export -f one
xargs -P 10 -I{} bash -c 'one "$@"' _ {} < srcs-i915.txt
bad=$(find logs-i915 -name "*.failed" | wc -l | tr -d ' ')
echo "sources: $n  compiled: $((n - bad))  failed: $bad"
for f in $(find logs-i915 -name "*.failed"); do cat "${f%.failed}.log"; done | grep -h "error:" | sed -E 's|^[^ ]*/([^/]+:[0-9]+):[0-9]+: error: |\1 |' | sed -E 's/^[^ ]+ //' \
    | sort | uniq -c | sort -rn | head -"${1:-40}"
