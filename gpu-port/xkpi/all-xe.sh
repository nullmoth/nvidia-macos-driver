#!/bin/bash
# all-xe.sh [N]: compiles every source Linux 6.14's xe Makefile builds (display included) into obj-xe/ and ranks the
# remaining errors. The DRM core, TTM, scheduler and dma-buf come from all.sh (obj/), as for i915.
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
V=$NULLMOTH_BUILD_ROOT/build/gpu-src/linux-6.14
XE=$X/xe-src
# the i915 display and soc code xe shares: drm-kmod's 6.14 copy, the one the i915 build already compiles
I915=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi/drm-kmod/drivers/gpu/drm/i915
# compat-i915-headers reach it as ../../../i915 from inside xe-src
ln -sfn "$I915" i915
cd "$X" || exit 1
# xe-src: the 6.14 xe tree with every GPL-licensed file and the KUnit tests removed, so none can be compiled or
# picked up by an #include "..." from its own directory; the replacements live in override/xe.
rsync -a --delete --exclude tests/ --exclude xe_pt_walk.c --exclude xe_pt_walk.h --exclude xe_res_cursor.h \
    --exclude 'xe_trace*' --exclude 'xe_vsec.*' --exclude xe_module.c "$V/drivers/gpu/drm/xe/" xe-src/ || exit 1
/opt/homebrew/bin/python3 fork_xe.py xe-src || exit 1
# (Makefile/Kconfig are kbuild files, never built; only source and headers must be free of GPL)
if grep -rl --include='*.c' --include='*.h' 'SPDX-License-Identifier: GPL' xe-src; then echo 'a GPL file is left in xe-src' >&2; exit 1; fi
mkdir -p obj-xe/gen logs-xe && rm -f logs-xe/*.log logs-xe/*.failed
# GPL-licensed files in 6.14 xe: never compiled into our kext. xe_pt_walk.c and xe_res_cursor.h are rewritten clean
# room (override/xe); xe_trace*.c are tracepoints (none on macOS); xe_vsec.c is PMT telemetry (not needed to run);
# xe_module.c is replaced by the port's own module glue (override/xe/nmx_xe_module.c).
EXCLUDE="xe_pt_walk.c xe_trace.c xe_trace_bo.c xe_trace_guc.c xe_trace_lrc.c xe_vsec.c xe_module.c"
# xe_heci_gsc.c publishes a child device for Linux's MEI driver; override/xe/nmx_xe_heci_gsc.c stands in for it
EXCLUDE="$EXCLUDE xe_heci_gsc.c"
# intel_display_trace.c: display tracepoints, which drm-kmod does not carry and macOS has no use for
EXCLUDE="$EXCLUDE intel_display_trace.c"
# the workaround tables are generated at build time from xe_wa_oob.rules / xe_device_wa_oob.rules by a host tool
if [ ! -f obj-xe/gen/generated/xe_wa_oob.h ]; then
    mkdir -p obj-xe/gen/generated
    xcrun clang -O2 '-Dprogram_invocation_short_name=getprogname()' -include stdlib.h -o obj-xe/gen/xe_gen_wa_oob "$XE/xe_gen_wa_oob.c" || exit 1
    obj-xe/gen/xe_gen_wa_oob "$XE/xe_wa_oob.rules" obj-xe/gen/generated/xe_wa_oob.c obj-xe/gen/generated/xe_wa_oob.h || exit 1
fi
/opt/homebrew/bin/python3 xesrc.py "$V" amd.flags --i915 "$I915" DRM_XE DRM_XE_DISPLAY | sed "s|^$V/drivers/gpu/drm/xe/|$XE/|" > srcs-xe.all
: > srcs-xe.txt
while read -r f; do
    case " $EXCLUDE " in
        *" $(basename "$f") "*) ;;
        *) echo "$f" >> srcs-xe.txt ;;
    esac
done < srcs-xe.all
for f in "$X"/override/xe/*.c; do [ -f "$f" ] && echo "$f" >> srcs-xe.txt; done
n=$(wc -l < srcs-xe.txt | tr -d ' ')
one() { b=$(basename "$1" .c); rm -f logs-xe/$b.failed; ./cc-xe.sh "$1" > logs-xe/$b.log 2>&1 || touch logs-xe/$b.failed; }
export -f one
xargs -P 10 -I{} bash -c 'one "$@"' _ {} < srcs-xe.txt
bad=$(find logs-xe -name "*.failed" | wc -l | tr -d ' ')
echo "sources: $n  compiled: $((n - bad))  failed: $bad"
for f in $(find logs-xe -name "*.failed"); do cat "${f%.failed}.log"; done | grep -h "error:" | sed -E 's|^[^ ]*/([^/]+:[0-9]+):[0-9]+: error: |\1 |' | sed -E 's/^[^ ]+ //' \
    | sort | uniq -c | sort -rn | head -"${1:-40}"
