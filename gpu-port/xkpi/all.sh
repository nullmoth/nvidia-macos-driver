#!/bin/bash
# all.sh [N]: compiles every source of the AMD render path in parallel and ranks the remaining errors (top N).
# Sources are exactly what drm-kmod's Makefiles build for amdgpu (minus the display core), drm, ttm and dmabuf,
# evaluated against amd.flags, minus files that are GPL-only or built on FreeBSD file descriptors or its console;
# the port supplies its own code for those.
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
K=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi
cd "$X" || exit 1
mkdir -p logs && rm -f logs/*.log logs/*.failed

# amdgpu_dm_crc.c: CRC capture is a debugfs feature (no debugfs on macOS; its header stubs the hooks to NULL)
EXCLUDE="amdgpu_dm_crc.c drm_gem_ttm_helper.c drm_writeback.c drm_hdcp_helper.c sync_file.c dma-fence-unwrap.c dma-buf.c dma-buf-kmod.c linux_fb.c vt_drmfb.c drm_os_freebsd.c"
/opt/homebrew/bin/python3 srclist.py "$K/drm-kmod" amd.flags amd/amdgpu drm ttm dmabuf 2>/dev/null > srcs.all
: > srcs.txt
while read -r f; do
    case " $EXCLUDE " in
        *" $(basename "$f") "*) ;;
        *) echo "$f" >> srcs.txt ;;
    esac
done < srcs.all

# DRM sources drm-kmod builds in other modules that the AMD driver also calls (MIT or GPL-2.0-or-MIT)
# drm_mipi_dsi.c: i915 drives DSI panels through it (MIT)
EXTRA="drm_client.c drm_client_modeset.c drm_panel_orientation_quirks.c drm_mipi_dsi.c"
for e in $EXTRA; do echo "$K/drm-kmod/drivers/gpu/drm/$e" >> srcs.txt; done
n=$(wc -l < srcs.txt | tr -d ' ')
xargs -P 10 -n 1 ./one.sh < srcs.txt
bad=$(ls logs/*.failed 2>/dev/null | wc -l | tr -d ' ')
echo "sources: $n  compiled: $((n - bad))  failed: $bad"
for f in logs/*.failed; do [ -e "$f" ] && cat "${f%.failed}.log"; done | grep -h "error:" | sed -E 's|^[^ ]*/([^/]+:[0-9]+):[0-9]+: error: |\1 |' | sed -E 's/^[^ ]+ //' \
    | sort | uniq -c | sort -rn | head -"${1:-40}"
