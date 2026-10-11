#!/bin/bash
# all-npucore.sh [N]: NMXNPU's DRM core - drm-kmod's drm and dmabuf modules, as all.sh selects them, plus the DRM
# helpers the NPU drivers use - compiled with CONFIG_DRM_ACCEL into obj-npucore/.
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
K=$NULLMOTH_BUILD_ROOT/build/gpu-src/kpi
cd "$X" || exit 1
mkdir -p obj-npucore logs-npucore && rm -f logs-npucore/*.log logs-npucore/*.failed
EXCLUDE="drm_gem_ttm_helper.c drm_writeback.c drm_hdcp_helper.c sync_file.c dma-fence-unwrap.c dma-buf.c dma-buf-kmod.c linux_fb.c vt_drmfb.c drm_os_freebsd.c"
/opt/homebrew/bin/python3 srclist.py "$K/drm-kmod" amd.flags drm dmabuf 2>/dev/null > srcs-npucore.all
: > srcs-npucore.txt
while read -r f; do
    case " $EXCLUDE " in *" $(basename "$f") "*) ;; *) echo "$f" >> srcs-npucore.txt ;; esac
done < srcs-npucore.all
for e in drm_client.c drm_client_modeset.c drm_panel_orientation_quirks.c; do echo "$K/drm-kmod/drivers/gpu/drm/$e" >> srcs-npucore.txt; done
n=$(wc -l < srcs-npucore.txt | tr -d ' ')
one() { b=$(basename "$1" .c); ./cc-npucore.sh "$1" > logs-npucore/$b.log 2>&1 || touch logs-npucore/$b.failed; }
export -f one
xargs -P 10 -I{} bash -c 'one "$@"' _ {} < srcs-npucore.txt
bad=$(find logs-npucore -name "*.failed" | wc -l | tr -d ' ')
echo "sources: $n  compiled: $((n - bad))  failed: $bad"
for f in $(find logs-npucore -name "*.failed"); do cat "${f%.failed}.log"; done | grep -h "error:" | sed -E 's|^[^ ]*/([^/]+:[0-9]+):[0-9]+: error: |\1 |' | sed -E 's/^[^ ]+ //' \
    | sort | uniq -c | sort -rn | head -"${1:-40}"
